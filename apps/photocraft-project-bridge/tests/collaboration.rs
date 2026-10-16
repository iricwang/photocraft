//! Process-isolated native clients against the real project-host executable.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use photocraft_engine::{
    Session,
    defold_project_model::ContentHash,
    defold_project_net::{self as net, HostTicket},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

fn hidden(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = command;
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct Host {
    process: Process,
    ticket: HostTicket,
}
impl Host {
    fn start(root: &Path, pairing: &Path, private: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_photocraft-project-bridge"));
        command
            .args(["--project", root.to_str().unwrap(), "--export-pairing", pairing.to_str().unwrap()])
            .env("LOCALAPPDATA", private)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hidden(&mut command);
        let mut process = Process(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(bytes) = fs::read(pairing)
                && let Ok(ticket) = serde_json::from_slice::<HostTicket>(&bytes)
                && net::call(&ticket, "/v1/describe", b"{}").is_ok()
            {
                return Self { process, ticket };
            }
            assert!(process.0.try_wait().unwrap().is_none(), "Fixture host exited before readiness");
            assert!(Instant::now() < deadline, "Fixture host did not become ready");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    fn stop(mut self) {
        net::call(&self.ticket, "/v1/host/shutdown", b"{}").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.process.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "Fixture host did not stop");
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

struct Client {
    process: Process,
    input: ChildStdin,
    replies: Receiver<Value>,
}
impl Client {
    fn start(profile: &Path) -> Self {
        fs::create_dir_all(profile).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", "collaboration_worker", "--nocapture", "--test-threads=1"])
            .env("PHOTOCRAFT_COLLAB_WORKER", "1")
            .env("LOCALAPPDATA", profile)
            .env("PHOTOCRAFT_CONFIG_DIR", profile)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        hidden(&mut command);
        let mut process = Process(command.spawn().unwrap());
        let input = process.0.stdin.take().unwrap();
        let output = process.0.stdout.take().unwrap();
        let (sender, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                if let Some((_, value)) = line.split_once("PHOTOCRAFT_RESULT ") {
                    let reply: Value = serde_json::from_str(value).unwrap();
                    if sender.send(reply).is_err() {
                        break;
                    }
                }
            }
        });
        Self { process, input, replies }
    }
    fn call(&mut self, value: Value) -> Value {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
        self.replies.recv_timeout(Duration::from_secs(20)).expect("Native worker reply timed out")
    }
    fn execute(&mut self, command: &str, params: Value) -> Value {
        self.call(json!({"command":command,"params":params}))
    }
    fn ok(&mut self, command: &str, params: Value) -> Value {
        let reply = self.execute(command, params);
        assert_eq!(reply["ok"], true, "{command}: {reply}");
        reply["result"].clone()
    }
    fn connect_open(&mut self, pairing: &Path) {
        self.ok("defold.connectProjectHost", json!({"pairingFile":pairing}));
        self.ok("defold.openProjectGui", json!({"path":"/main.gui"}));
    }
    fn inspect(&mut self) -> Value {
        self.call(json!({"inspect":true}))["result"].clone()
    }
    fn draw(&mut self, name: &str, color: &str) {
        self.ok("layer.new.layer", json!({"name":name}));
        self.ok("paint.stroke", json!({"points":[[36,48],[60,48]],"size":10,"color":color}));
    }
    fn reset(&mut self) {
        self.ok("defold.releaseProjectEditing", json!({}));
        assert_eq!(self.call(json!({"reset":true}))["ok"], true);
    }
    fn stop(mut self) {
        assert_eq!(self.call(json!({"quit":true}))["ok"], true);
        assert!(self.process.0.wait().unwrap().success());
    }
}

#[test]
#[ignore = "private helper launched only by the process-isolated collaboration test"]
fn collaboration_worker() {
    assert_eq!(std::env::var("PHOTOCRAFT_COLLAB_WORKER").unwrap(), "1");
    let mut session = Session::new();
    for line in std::io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let result = if request["quit"] == true {
            println!("PHOTOCRAFT_RESULT {}", json!({"ok":true}));
            std::io::stdout().flush().unwrap();
            break;
        } else if request["reset"] == true {
            session = Session::new();
            Ok(json!({"reset":true}))
        } else if request["inspect"] == true {
            let state = session.active().unwrap();
            let bytes = photocraft_format::save_to_bytes(&state.doc, &Default::default()).unwrap();
            let design = state.doc.defold.as_ref().unwrap();
            Ok(json!({"nativeHash":ContentHash::sha256(&bytes),"documentId":state.doc.id.0,
                "revision":state.revision,"savedRevision":state.saved_revision,
                "activeLayer":state.active_layer.map(|layer|layer.0),"project":design.project,
                "source":photocraft_engine::defold_project_client::status(&session)}))
        } else {
            session.execute(request["command"].as_str().unwrap(), request["params"].clone())
        };
        let reply = match result {
            Ok(result) => json!({"ok":true,"result":result}),
            Err(error) => json!({"ok":false,"error":error.to_string()}),
        };
        println!("PHOTOCRAFT_RESULT {reply}");
        std::io::stdout().flush().unwrap();
    }
}

#[test]
fn two_native_clients_handoff_conflict_offline_draft_and_host_restart() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("game");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("game.project"), "[display]\nwidth=96\nheight=96\n").unwrap();
    let original = "nodes { id: \"root\" type: TYPE_BOX size{x:96 y:96} position{x:48 y:48} alpha:0 }\n";
    fs::write(root.join("main.gui"), original).unwrap();
    let pairing = directory.path().join("pairing.json");
    let private = directory.path().join("host-private");
    let host = Host::start(&root, &pairing, &private);
    let old_address = host.ticket.address;
    let mut first = Client::start(&directory.path().join("first-client"));
    let mut second = Client::start(&directory.path().join("second-client"));
    second.connect_open(&pairing); // deliberately retain a source-less stale draft
    first.connect_open(&pairing);
    first.ok("defold.requestProjectEditing", json!({}));
    first.draw("ArtistA", "#e64868");
    let saved = first.ok("defold.saveDesignSource", json!({}));
    assert_eq!(saved["saved"], true);
    let first_snapshot = first.inspect();
    second.draw("StaleArtistB", "#00cc66");
    let stale = second.inspect();
    let denied = second.execute("defold.saveDesignSource", json!({}));
    assert_eq!(denied["ok"], false);
    assert!(denied["error"].as_str().unwrap().contains("Lease"), "{denied}");
    assert_eq!(second.inspect()["nativeHash"], stale["nativeHash"]);
    first.ok("defold.releaseProjectEditing", json!({}));
    let conflict = second.execute("defold.saveDesignSource", json!({}));
    assert_eq!(conflict["ok"], false);
    assert!(conflict["error"].as_str().unwrap().contains("SourceConflict"), "{conflict}");
    assert_eq!(second.inspect()["nativeHash"], stale["nativeHash"]);
    let recovery = directory.path().join("stale-draft.pcraft");
    second.ok("file.saveACopy", json!({"path":recovery}));
    let recovered = photocraft_format::load_from_bytes(&fs::read(&recovery).unwrap()).unwrap();
    assert!(recovered.layers.iter().any(|layer| layer.name == "StaleArtistB"));
    second.reset();
    second.connect_open(&pairing);
    assert_eq!(second.inspect()["project"], first_snapshot["project"], "Native stable layer/node identities must survive a different client");
    second.draw("ArtistB", "#198de6");
    second.ok("defold.saveDesignSource", json!({}));
    let before_disconnect = second.inspect();
    host.stop();
    second.draw("OfflineArtwork", "#cc9900");
    let offline = second.inspect();
    assert_ne!(offline["nativeHash"], before_disconnect["nativeHash"]);
    assert_eq!(second.execute("defold.saveDesignSource", json!({}))["ok"], false);
    assert_eq!(second.inspect()["nativeHash"], offline["nativeHash"]);
    assert_eq!(second.inspect()["source"]["unsaved"], true);
    let offline_copy = directory.path().join("offline-draft.pcraft");
    second.ok("file.saveACopy", json!({"path":offline_copy}));
    assert!(photocraft_format::load_from_bytes(&fs::read(offline_copy).unwrap()).unwrap().layers.iter().any(|layer| layer.name == "OfflineArtwork"));
    let host = Host::start(&root, &pairing, &private);
    assert_ne!(host.ticket.address, old_address, "Exercise a changed host endpoint");
    second.ok("defold.connectProjectHost", json!({"pairingFile":pairing}));
    assert_eq!(second.inspect(), offline, "Reconnect must retain document, dirty state, selection, native revision and known source head");
    second.ok("defold.saveDesignSource", json!({}));
    let final_source = second.inspect();
    first.ok("defold.connectProjectHost", json!({"pairingFile":pairing}));
    assert_eq!(first.inspect(), first_snapshot, "Another client's reconnect must not reload its open draft");
    first.reset();
    first.connect_open(&pairing);
    assert_eq!(first.inspect()["project"], final_source["project"]);
    // A different connection must not redirect an already-open document's saves.
    let other_root = directory.path().join("other-game");
    fs::create_dir_all(&other_root).unwrap();
    fs::write(other_root.join("game.project"), "[display]\nwidth=96\nheight=96\n").unwrap();
    fs::write(other_root.join("main.gui"), original).unwrap();
    let other_pairing = directory.path().join("other-pairing.json");
    let other_host = Host::start(&other_root, &other_pairing, &private);
    let before_switch = second.inspect();
    second.ok("defold.connectProjectHost", json!({"pairingFile":other_pairing}));
    assert_eq!(second.inspect(), before_switch);
    second.ok("defold.saveDesignSource", json!({}));
    assert_eq!(fs::read_to_string(other_root.join("main.gui")).unwrap(), original);
    assert!(!other_root.join(".photocraft/sources/index.json").exists());
    other_host.stop();
    let preview = first.execute("defold.syncDesign", json!({}));
    assert_eq!(preview["ok"], false, "No running Defold means publication cannot succeed");
    assert_eq!(fs::read_to_string(root.join("main.gui")).unwrap(), original);
    assert!(!root.join("photocraft_generated").exists(), "Source handoff must not produce game assets");
    first.stop();
    second.stop();
    host.stop();
}

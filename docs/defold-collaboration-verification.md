# Defold 协同版本交付与验收

日期：2026-10-16。

## 使用入口

- `Start-FortySix-PhotoCraft.cmd`：打开角色 GUI。
- `Start-FortySix-PhotoCraft-Challenge.cmd`：打开挑战 GUI。
- 两个入口均使用 `target/debug/photocraft-project-collab-reconnect.exe`，项目桥接程序在同一目录；旧版程序保留。
- 设计源文件保存到工程的 `.photocraft` 管理目录；点击“同步到 Defold”才发布游戏资源。绘制与移动不触发游戏资源写入。

详细使用与数据管理说明见 [defold-gui.md](defold-gui.md)。多人通过工程桥接配对、编辑权交接和冲突审核协作；共享素材保留原生创作源，消费者引用固定版本，可打开源文件编辑或转为独立素材。

## 版本要求

配套 Defold 为 `1.14.1-patch.18`。补丁位于：

`X:/AI/UI/defold/editor/target/craft-collab/workspace/editor/target/editor/Defold-x86_64-win32-patch-16-to-18.zip`

SHA-256：`97b46a458110b8410665b820203acaf8e282d666af76f1a80a8ecdd2c3fd326f`。

使用压缩包随附的补丁助手。当前 `X:/Programs/Defold` 仍为 patch.16，本次未覆盖正在使用的安装。

## 验证证据

- 引擎完整回归：1905 通过、0 失败、14 跳过；随后发现的启动连接和旧资源兼容修复另作定向验收。
- Defold：35 项测试、225 个断言通过；最终编译包的工程连接、两份 GUI 快照及 GUI/atlas/PNG 资源打开检查通过。
- 隔离桌面窗口已实际打开 `/assets/defold/gui/ui_window_challenge.gui`；原有文档、图层、工具与生图面板可见，Defold 属性和同步面板独立。
- Windows 实际 TLS 连接已通过。接收连接显式恢复为阻塞模式，避免继承监听器的非阻塞状态而中断握手。
- 项目桥接定向回归 `paired_tls_connection_uses_the_production_project_router` 通过（7 次实际 TLS RPC，覆盖工程描述、GUI 解析、身份、资产与历史查询）；证据：`target/collaboration-tls-final.log`。
- 旧管理纹理缺少资产记录、导入节点尚未赋予资产身份时重新生成局部导出信息的定向回归通过；证据：`target/collaboration-legacy-regression.log`。最终回归夹具针对带纹理的素材节点，采用测试包 `opt-level=0`；实际交付程序和真实 GUI 联调仍使用已构建的优化版本。
- 首次启动等待改为有限的 10 秒，桥接提前退出会报告错误。旧管理纹理首次发布会重新生成节点局部导出信息，包括尚无资产身份的导入节点。
- 最终桌面程序和桥接已构建完成；关闭测试桥接后从启动入口直接打开挑战 GUI 成功，文档尺寸为 2340×1080。界面证据：`target/collaboration-ui-profile/final-gui.png`，检查数据：`final-inspect.json`。

最终编译版 Defold 的实际发布闭环已通过：

| 测试 | 结果 | 证据 |
| --- | --- | --- |
| 挑战 GUI：新增、绘制、仅保存源文件、同步、改名、撤销/重做、删除、重新打开 | 通过，24.86 秒 | `target/collaboration-challenge-live.log`；首次事务 `270e4f54-5891-4f69-893b-e88858a85d40` |
| 角色 GUI：相同完整流程 | 通过，23.67 秒 | `target/collaboration-character-live.log`；首次事务 `deb018b5-335c-46da-bb93-f02df3898cee` |
| 两份真实 GUI 共享原生素材：统一更新、统一改名、消费者画笔保护、发布范围审核、转为独立素材、删除 | 通过，40.63 秒 | `target/collaboration-shared-live.log` |

这些测试确认源文件保存不写游戏 GUI、改名保持资产身份、撤销/重做不增加资产路径、删除再撤销仍恢复同一素材。它们通过实际编译版 Defold 的适配器完成发布，不以模拟服务替代。

## 后续：独立客户端接力与重连

新增 `apps/photocraft-project-bridge/tests/collaboration.rs`，启动真实工程桥接，以及两个使用独立缓存、独立客户端身份的原生 Session 进程。测试通过，执行耗时 7.98 秒；证据为 `target/collaboration-two-clients.log`。

该用例验证：

- A 持有编辑权时，B 不能写入；A 交接后，B 的旧源版本仍被 CAS 拒绝，双方草稿不被替换。
- 保存原生副本后，另一客户端能读取已确认的完整分层源和稳定图层/节点身份，继续绘制与保存。
- 断开主机后保存失败，原生内容、选中状态和脏标记保留；另存 `.pcraft` 可保留包含新图层的可编辑副本。
- 主机换地址重启后，重新连接更新当前工作区所有现有文档的连接，保持原生内容指纹、选中图层、文档版本和已知源头不变；随后保存成功。
- 连接另一个工程不会把已打开文档的保存目标切过去；全过程原始 GUI 字节不变，也未生成游戏贴图。

修复位于 `defold_project_connection.rs`。同工作区有未结束的保存或发布任务时，需要先等待任务结束，以免旧任务回执把连接改回过期地址。

复验命令：`cargo test -p photocraft-project-bridge --test collaboration`。私有 worker 测试默认跳过，不应单独运行。

本次同时补齐网页版编译检查（25 项通过）、依赖分层检查（31 个 crate 通过）及协作测试静态检查（严格警告模式通过）。

修改命令后的异常参数门禁 `no_command_panics_or_hangs_on_adversarial_params` 已通过，执行耗时 17.97 秒；证据为 `target/collaboration-followup-panic.log`。

重连修复版已构建并更新两个启动入口。程序为 `target/debug/photocraft-project-collab-reconnect.exe`，SHA-256 为 `fd841db579e137f489bee9984e1754b2d63657075d87e98298ab9f4436cabc7d`；原有协同版保留。引擎静态检查通过，保留一项与本次无关的既有警告（`color_to_alpha_cmds.rs:68`），没有将整个引擎宣称为严格零警告。

## 已完成发布的回执恢复（2026-10-16）

真实 `ui_window_challenge.gui` 发布出现过 Defold 已落盘为 `COMMITTED`、PhotoCraft
仍停在 `APPLYING` 的情况。原因是客户端只短暂查询执行状态，下一次手动同步却直接
拒绝旧的非终态记录，没有先向权威端确认它是否已经完成。

手动同步现在会先确认旧事务；完成后再发布当前画稿，不重新应用旧计划。Defold
已经关闭时，主机可以读取它持久化的 `COMMITTED` 回执；仍需校验原事务 ID、计划哈希、
每个 GUI 和资源的实际保存哈希，再正常归档并提交资产目录。未完成、身份不符或
输出被修改的回执不能被当作发布成功。恢复仅由显式同步或状态检查触发。

新增回归覆盖丢失回执后的再次手动同步、Defold 关闭后的回执确认，以及输出发生
外部修改时拒绝确认。旧回执不会替换较新的本地像素，也不会把旧计划再应用一次。
发布客户端 23 项、事务持久化 20 项、编辑器协议 2 项测试通过。

现场事务 `4a570752-6d84-4454-a9a9-4c8c1b4f13c9` 已使用同一恢复路径确认为
`COMMITTED`，4 个输出的前后哈希一致。该步骤只补齐 PhotoCraft 的归档及资产目录，
没有重写游戏 GUI 和贴图。恢复前快照及确认回执保存在
`X:/AI/UI/deliverables/photocraft-publication-recovery-4a570752/`。

维护入口为 `cargo run -p photocraft-engine --example recover_defold_publication -- <project-directory> <transaction-id>`。
它只查询和确认指定的既有事务，不创建或应用新计划。

## 验收边界

所有写入联调使用有明确测试标记的隔离工程 `X:/AI/UI/defold/tmp/craft-adapter/packaged-final-game-project`。原游戏 `X:/Projs/FortySix-MiniGame` 在此次收尾中未被写入。

两个独立原生进程的真实 TLS、接力编辑及主机重启验收已完成，跨两台真实电脑的网络和防火墙验收仍未完成。最终共享改动后的性能及素材语料门禁尚未重新完成；不能据早期检查宣称最终版本全部通过。断线期间关闭客户端后的自动恢复与冲突续接仍需单独验收，原生副本检查不等于已验证自动恢复。

资源删除采用保守保留策略。旧路径供 Defold 撤销与历史版本恢复使用，暂不自动永久清理。多个文件写入和多个创作源保存并非操作系统级原子事务，失败依靠持久记录恢复；原生编辑器撤销不会撤回 PNG 文件字节。

共享创作目前不支持同包自引用、循环引用和依赖画布坐标的自定义图案；非默认的源节点整体颜色、透明度和隐藏状态会阻止共享发布，避免改变所有消费者。具体限制见使用说明。

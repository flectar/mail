# 验收记录

初始记录：2026-10-10。源基线：`bc0f966846a6d1758bb39ea54a24be4de6cad8d7`。
更新：2026-10-11，累计真实邮箱、平板交互、第四轮CPU性能修复结果，并保留RS时间戳列的历史口径更正。

最新摘要见 [当前状态](CURRENT-STATUS.md)，构建／签名／安装步骤见 [移植指南](README.md)。本表按累计执行范围保留历史包与主机结果；不是第四包逐项重新执行的完整回归，也不表示四阶段全部完成。`target/ohos-evidence/` 与 `target/ohos-host-contracts/` 是本地忽略目录，表中的证据文件名用于本机追溯，不随源码提交；关键数字及限制保存在本页和 [动画性能记录](ANIMATION-PERFORMANCE.md)。

先验证的设备为 MatePad Pro（WEB-W00，HarmonyOS 7／API 26）。IMAP／SMTP 测试配置于2026-10-11由用户提供并留在本机，不随仓库提交。HDC 的连接端点已核对为同一授权设备，不公开设备标识。本次未安装 Simulator；早期启动的既有实例已关闭。

**性能记录更正：** 历史schema 1读取RS第一列，[公开Rosen源码](https://github.com/openharmony/graphic_graphic_2d/blob/master/rosen/modules/render_service_base/src/gfx/fps_info/rs_surface_fps.cpp) 定义输出为 `flushTime:presentTime`。以下旧数值与JSON原样保留，速率及间隔均改按提交时间解释；实际呈现FPS提升及原短采集30 FPS通过结论撤回。独立的复制耗时、主机契约、安装、交互功能、生命周期和像素验证继续保留。schema 2改用第二列并保留双列；公开master尚未与设备RenderService版本匹配。第三、四包已按真实输入边界独立复算，仍不以候选首帧或短区间平均值证明完整动作流畅。详见 [性能说明](PERFORMANCE.md)、[测量口径](MEASUREMENT-SCHEMA.md)及 [最新动画结果](ANIMATION-PERFORMANCE.md)。

| 检查 | 结果 | 说明 |
|---|---|---|
| Rust/SDK 工具基线 | 通过 | Rust 1.97，DevEco 26.0.0.851，API 26.0.0.105；OHOS ARM64 target 已安装 |
| Stage/ArkTS 编译 | 通过 | 实际 `default@CompileArkTS`，API 26 SDK |
| 包元数据 | 通过 | bundle `moe.kiwi.FlectarMail`，compatible API20，phone/tablet/2in1 |
| C++ 桥接/图形/输入头文件 | 通过 | SDK clang ARM64 `-Wall -Wextra` 检查；API26 专属输入开关改为动态可选 |
| Slint CPU 和输入后端测试 | 通过 | 5 项：UTF 边界/真实 TextInput 预编辑、提交、选区和删除/软件渲染帧/无系统后端的默认字体查询；含 emoji surrogate 场景 |
| 键盘隐藏/重新显示契约 | 平板通过 | 当前生产backend/native源5项实际Slint1.18.1测试通过；新包实际Back收起IME、同搜索框再点击重开，纯英文搜索与拼音候选提交无重复；普通文本/光标Update不重开。中文可见预编辑仍独立未通过 |
| 原生 postIME 未消费键回退 | 平板通过 | 官方 WindowSession/ArkUI 源确认已消费键不进入普通XComponent callback；删除已attach时丢弃ASCII/Edit/Nav的额外gate。新包实际英文搜索找到合成线程，系统拼音候选只提交一次。审查 input-key-routing-audit.md |
| 返回键隐藏输入法 | 平板通过 | 新包实际Back后窗口恢复可见高度，应用仍前台且保留搜索内容；只隐藏键盘，继续保留编辑器Attach。同框重开另验 |
| OpenSSL OHOS 原生编译 | 通过 | 官方 3.5.9，ARM64 与 x86_64，静态 libssl/libcrypto、headers、license、manifest |
| Core 启动/后台门控回归 | 通过 | 4 项实际主机测试：空账户与启动失败保留、SMTP暂停不claim队列、晚启动worker和retained事件门控；真实协议后台行为待测试账户 |
| OpenPGP / GnuPG 互操作回归 | 通过 | 11 项实际主机测试：双向 MIME签名/加密、匿名收件人、密文篡改、过期/吊销/错误身份/缺少Bcc密钥、受保护私钥导入及重启；Asset与真机互操作仍待验证 |
| 数据库快照 ZIP 边界测试 | 通过 | 3 项独立执行：manifest/附件内容、符号链接拒绝与清理、不覆盖已有文件 |
| 打印 PDF 文件协议 | 通过 | 实际 writer 的独立契约测试：pypdf strict 解析 3 个 JPEG 页面并检查像素、已有文件不覆盖、取消后删掉部分输出；尚未验证真机打印与完整 Blitz 排版 |
| 邮件附件原文件名契约 | 主机通过 | 修复系统导入缓存名前缀进入 MIME；实际 ImportedDocument 源6项测试通过，含真实 Core MIME构建/解析/提取「旅行预算 2026 📨.pdf」与字节回环、取消清理与非法路径名拒绝。修复包真机导入发送另验 |
| Files 导入原文件名契约 | 主机通过 | 7项真实SQLite/WebDAV/JMAP HTTP契约通过：原名独立保存为displayName，暂存upload，中文/空格/emoji和MIME、离线重试、旧队列兼容、原覆盖条件与非法名称；实际远端协议账户及真机上传另验 |
| 持久邮件通知 dispatch | 主机通过 | 生产 dispatcher 与真实 Core/SQLite 的8组契约及2项单测通过，覆盖自动邮件、范围/分类等待、失败重试、崩溃恢复及活动窗口边界；OS请求与生命周期为mock，不计真机通知通过 |
| 完整 Rust ARM64 交叉编译 | 通过 | 实际 debug `libfeihong_rust.a` 构建成功，包含原有 Slint/Blitz 应用与 OHOS 平台服务；HAP 共享库链接单独验证 |
| OHOS 增量构建 | 通过 | 修复生成 Slint overlay 与缺失可选 .env 被当成输入导致的重复构建；实际标准 ARM64 debug 构建4m50s，随后无改动构建1.49s（2026-10-11），日志 target/ohos-noop-validation.log |
| MatePad CPU 性能基线 | 已观察严重卡顿／历史提交统计 | 原未优化debug包2880×1786，同HTML的8次手势（含命令间空窗）：RS第一列35槽/10.015s＝3.395次提交／秒，提交间隔中位296.969ms、p95 467.930ms（统一nearest-rank）；旧actual_present_fps误命名，不计实际呈现验收。静止CPU约0%，滚动约单核，RSS560MB。perf-baseline-analysis.json |
| 性能问题探测 | 平板热点与分段探测通过 | 使用RS本应用Surface导出、仅自身PID的100Hz用户态hiperf采样2648条/0丢失，以及2秒聚合Native阶段计时；probe旧第一列3.50次提交／秒，原生提交26–31ms（Map+复制约25ms、fence约0.06ms）。原符号重建.text与签名包字节完全相同；不重叠self采样59.587%属于Slint遍历、生成UI分派和软件绘制入口，最高单项17.92%为item visitor。此热点与耗时证据独立保留，不把提交速率作实际呈现FPS。perf-baseline-detailed-hotspots.json；实际修复效果另验 |
| CPU 性能修复回归 | 主机与安装通过／呈现待复核 | OHOS调试绘制crate启用优化，持久Opaque RGBA逐行全屏复制保留stride和buffer轮转语义，事件队列4ms/128预算且不丢FIFO；实际生产14项及run_event_loop 2项测试通过。优化ARM64构建17m18s、增量确认2.31s、HAP签名安装通过；相同UID1/2880×1786/8次滑动，旧RS第一列11.247次提交／秒、提交间隔median36.188ms（原296.969ms），手势间隙计入统计和p95。另长手势提交区间1.877s为27.698次／秒，不代表整段5s或实际呈现FPS。failed=0、中文/彩色emoji实际正常；Native提交路径约7–8ms，Slint绘制p50约18–26ms；独立分段和功能结果保留。matepad-perf-optimized-10s.json、matepad-perf-optimized-5s.json |
| UI线程QoS实机 | API及像素通过／呈现增益待复核 | API设置实际返回0、Get等级5；前台UI线程设交互等级，后台Reset返回0、恢复前台再次等级5。同HTML截图逐像素一致。旧第一列固定8次手势11.437次提交／秒、另1.775s提交区间28.738次／秒，不能作为呈现增益或主要改善归因。perf-qos-background-resume.json、matepad-perf-qos-10s.json、matepad-perf-qos-5s.json |
| 提前预约VSync实机 | 主机契约与安装通过／呈现待复核 | 保留QoS、渲染优化和内容，仅把VSync预约前移到CPU绘制之前。ARM64 Rust构建1m05s、签名HAP19.156s/install-r/冷启动通过；同UID1/100%/2880×1786/8次手势，旧RS第一列151槽/8.996s＝16.674次提交／秒（原35槽/10.015s＝3.395），提交间隔median21.393ms（原296.969），p95含手势间隙415.167ms。相同长手势78槽/1.789s提交区间43.048次／秒、median20.930ms、p95 28.688ms，不代表请求5s或实际呈现。Slint render p50约14.7–15.5ms、Native提交路径约5ms、failed=0；生产/事件/FIFO/pacing20项主机契约及旧post-present负对照通过。matepad-perf-pacing-10s.json、matepad-perf-pacing-5s.json、early-vsync-host-validation.json |
| 光标小范围重绘瓶颈探测 | 平板通过 | 同一2880×1786 Surface，空主题光标稳态pixelCopy墙钟99.63/97.04ms、线程CPU99.43/96.85ms；HTML滚动复制墙钟/CPU均1.36ms。两场景stride11520、实际bufferUsage1547一致；请求usage为515，未启用READ_OFTEN。证据确认低频整屏复制开销发生在线程上，但不能据此断言缓存或DVFS原因。READ_OFTEN对照无改善，已撤回。perf-copy-cpu-probe-ime-isolated.json、matepad-perf-copy-cpu-probe-5s.json |
| Native缓冲损伤累计与引用管理 | 主机/平板通过当前场景 | 当前生产NativeDamageTracker/CopyRgbaDamage的13项测试及ASan/UBSan全部通过，含1200次任意轮换、各阶段失败与空损伤重试、stride/padding、尺寸/generation隔离、LRU、引用失败、后台释放与空源恢复。最大8条缓存持有公开NativeBuffer引用以保持对象存活，成功Unmap+Flush才清除该缓冲损伤；引用获取失败整屏复制且不缓存。当前生产Rust23项含generation、相同像素数宽高互换与密度变化时完整重绘；OHOS ARM64 Debug/Release语法检查和独立审查通过。真机光标、HTML滚动操作及画面、窗口resize和后台恢复通过，不代替滚动流畅验收；Literal Surface销毁重建未观察。native-damage-host-tests.json、damage-host-contracts/validation.json、native-damage-source-review/audit.json |
| 缓冲损伤历史包固定操作性能 | 历史提交对照保留／呈现结论撤回 | 同HTML/100%/2880×1786/同8次手势，旧第一列165槽/8.966s＝18.290次提交／秒，原3.395，约5.39倍是提交统计差异；提交间隔median19.170ms、p95 336.482ms、max805.624ms，保留命令空窗和全部长间隔。另84槽/1.804s提交区间46.013次／秒、p95 25.846ms，旧跨度仅为请求5s的36.08%。不据此确认实际呈现FPS或流畅。独立Slint绘制p50约14.67–15.41ms、Native提交路径约3.47–3.72ms、failed=0仍保留。matepad-perf-damage-10s.json、matepad-perf-damage-5s.json |
| 缓冲损伤历史包光标复制对照 | 平板通过 | 同一空主题及5段操作，测量期间不截图/OCR，按成功提交数加权：旧11窗口47次pixelCopy93.26ms／CPU92.86ms，Native提交路径104.22ms；新含暖机11窗口51次4.07ms／CPU4.04ms，Native15.05ms；新稳态10窗口47次2.29ms／CPU2.26ms，Native13.12ms。首次暖机25.10ms没有删除；前序HTML结转窗口另分类且全数保留。尺寸/stride/usage/有效CPU样本一致。光标正常低频闪烁不算性能失败。perf-damage-ime-isolated.json、perf-independent-damage-metric-audit.json |
| 可重复性能检测与回归 | 主机回归保留／旧实机FPS通过撤回 | 旧schema 1的31项检测单测执行通过，但第一列误命名为actual_present_fps。最终包4次2秒旧检测：第1/3次49.897次提交／秒/2.184s、50.180次提交／秒/2.212s，原JSON的pass保留，实际呈现30 FPS及短动作流畅通过结论撤回；第2次仅1槽、第4次提交跨度1.173s覆盖58.67%，原inconclusive保留。当前schema 2第二列与双列保留方式见测量口径，设备源码版本未匹配，呈现验收待复核。独立生产Rust23项及两组应失败负对照通过仍有效。matepad-perf-damage-detector-{1,2,3,4}.json、ohos-perf-unit-tests.json、damage-host-contracts/validation.json |
| 缓冲损伤历史包交互、画面及默认诊断 | 平板功能／像素通过当前场景 | 真实搜索输入/清除、Back收起与再开IME、邮件切换功能通过，动画流畅另验；HTML指定裁剪696500像素与前包、窗口/后台恢复后均0差异，此结论不扩展到整屏。静止RSS约520MB（前包519MB）、长手势约523MB，不代表峰值或合成器全部分配。最终签名包167057635字节、HAP构建20.379s，安装后不带performance参数冷启动PID12765，实际2次重绘后性能/QoS汇总0条，诊断默认关闭；原账户保留，手动同步显示完成。perf-damage-final-validation.json保留原样，其中旧FPS／阈值通过按本文撤回；perf-damage-build.json、perf-damage-default-off.json、perf-damage-html-pixel-comparison.json、perf-damage-surface-background-pixel-comparison.json |
| 离线 UI 样例数据 | 主机通过 | 实际 Core integration：1账户、5线程、6邮件/正文、1 PDF，原 GetThread/GetAttachment 查询、幂等保留阅读/星标、拒绝非样例数据库；样例协议工作暂停。真机界面单独验收 |
| PDFium OHOS ARM64 构建 | 通过 | Chromium/7881 固定源码，约19MiB；AArch64、动态依赖与公共FPDF导出检查通过 |
| PDFium 真机 C ABI 探针 | 通过 | 签名 debug HAP 冷启动实际 create/save/reload/text/render；1 页、11 字符、2100 个红色像素、1015 字节。临时 ELF 执行受系统限制，未更改设备安全设置 |
| 原邮件附件 PDF 预览 | 平板通过 | 原Slint附件UI实际打开两页样例：第1页蓝色133%、下一页第2页绿色159%，翻页与放大实际像素确认；网络附件下载与系统导出另验 |
| 真实邮件 PDF 下载与预览 | 部分通过 | UID1的原邮件UI显示3个原名附件，PDFium实际打开从IMAP下载的2页PDF，第1页蓝色133%像素确认（matepad-real-pdf-preview-redacted.png）；网络来信刷新时曾关闭预览并切走当前会话，尚待排查。翻页、系统导出、重导入与SMTP附件字节回环单独验收 |
| 本应用 DevEco 签名配置 | 通过 | 新建专属 debug profile/p12/cer，bundle 匹配且授权 UDID 包含当前平板；本地配置忽略，失败恢复测试通过 |
| 飞鸿邮箱签名 HAP/安装 | 通过 | 完整 ARM64 库真实链接、签名 HAP 构建及 MatePad 安装成功；不将安装成功记为运行通过 |
| 飞鸿邮箱启动/邮件列表首帧 | 通过 | MatePad Pro 冷启动 PID62078，2880×1786 Surface；217字体family、CJK fallback=true，原有三栏 Slint 邮件列表显示5个中文/emoji样例线程。Stage UIContext 初始化错误已修复；首帧截图保存在 target/ohos-evidence/matepad-first-frame.png |
| MatePad 邮件正文与触控 | 通过 | 触控选取原邮件，Blitz显示中文表格/配色/列表/代码并滚动；截图 target/ohos-evidence/matepad-complex-html.png、matepad-html-scrolled.png |
| MatePad 收件箱邮件卡片操作栏布局 | 平板通过 | 修复分隔线未设置y而被默认居中、穿过回复图标的问题：固定x/y为0；同操作栏改为stretch，让弹性空白将浏览器/信息按钮推到右侧。Slint1.18.1 ARM64完整Rust构建17m32s、签名HAP36.902s，167057626字节，安装与正常启动PID41158通过；同自身合成回复线程实机前后截图及独立视觉复核通过。悬浮窗1008×1714/generation2→全屏2880×1786/generation3，窄版三个回复按钮正常，宽版左右分组正常；这只记录resize，不代替Surface销毁重建。性能诊断关闭；本轮未重测FPS，既有perf-damage-signed.hap的schema 1数字属于提交统计。ui/views/mail-reading-pane.slint；inbox-layout-card-actions-{before,after}-public.png、inbox-layout-{floating,restored}-actions-public.png、inbox-layout-validation.json |
| 动画逐动作探测与第三包修复 | 明显改善／首次邮件停顿未解决 | 核对原生输入与RS时钟，双列及全部间隔保留；补齐实际命中的SVG／字体依赖优化、CPU大面积不透明动画及设置首次构建。第三包安装通过；三轮18场抬起后候选无>50ms，设置打开25–33ms、短HTML约50ms，但三次冷启动首次邮件仍等待154–163ms、事件窗口216–226ms。真实Scrollbar反向操作闭区间内最大24.947ms，完整跨抬起398.620ms仍保留，不推定惯性或全程流畅。静态中文／自身HTML像素对照通过，正常诊断关闭、账户保留；详见 ANIMATION-PERFORMANCE.md |
| 字体预热非阻塞与诊断契约 | 主机／OHOS／平板当前范围通过 | 第四包后台邮件字体构造、界面get/clone已完成模板及默认关闭数字诊断；6项实际源码host案例、3项应失败负对照、OHOS debug/release两项metadata及完整Rust/HAP构建安装通过。三次首用均ready1，界面bootstrap0.14–0.31ms、首候选等待56–94ms，原154–163ms；暖态六场单轮保留HTML58ms残余间隔，不称完全无卡顿。真实滚动／中文emoji／静态像素通过；最终normal PID29255诊断0、数据保留、强制亮屏关闭。检测脚本55项自测、72份历史双列及6个动作边界案例统计保持；详见 ANIMATION-PERFORMANCE.md、animation-font-prewarmed-normal-mode-validation.json |
| MatePad 中文输入与编辑 | 部分通过 | 实际系统软键盘拼音候选提交，正文中文/emoji提交、Ctrl+A选区替换、Ctrl+C/V复制粘贴、Ctrl+Z撤销；主题预编辑未显示，尚待定位IMF回调，不记完整中文输入通过 |
| 邮件正文 CPU PNG 字形 | 主机/平板通过 | 实际glifo0.2/VelloCPU0.1对照：PNG字形关解码为0像素，开解码为8919可见/8568彩色像素；OHOS target补齐png feature。新包normal启动统计已正常加载2个CBDT字体，真实SMTP邮件正文显示🕊️/📬/family ZWJ彩图，截图matepad-smtp-body-updated-redacted.png；root已目视检查 |
| MatePad 保存草稿/冷启动恢复 | 通过 | 离线配置实际保存→force-stop→冷启动，草稿主题正文从原SQLite恢复；真实账号草稿发送另验 |
| MatePad 前后台/窗口尺寸变化 | 平板通过当前场景 | 最终损伤修复包同PID5442，悬浮窗1008×1714/generation2→全屏2880×1786/generation3完整重绘；Home后恢复同进程，QoS恢复Set0/Get5、正文裁剪无变化。本次未采到Reset行，先前QoS包独立记录Reset0。perf-damage-floating-surface.json、perf-damage-restored-full-surface.json、perf-damage-background-resume.json |
| MatePad Surface销毁重建/横竖屏/分屏 | 待验证 | 尺寸及generation变化不能代替已观察到SurfaceDestroyed+SurfaceCreated；真实横竖屏、分屏与完整生命周期仍须单独验收 |
| PC/手机三端回归 | 待验证 | 尚无对应设备 |
| IMAP/SMTP 端到端 | 部分通过 | 用户于2026-10-11提供本机 .tmp 测试配置，使用严格 TLS 校验与仅向自身发送的合成邮件；主机协议与平板应用结果分别记录，不保存凭据 |
| 测试账户 IMAP 主机连接 | 通过 | 配置服务器993，证书校验TLS1.3、认证、只读INBOX/STATUS通过；无已有正文输出，记录 host-imap-readonly.json |
| 测试账户平板配置与同步 | 通过 | 正常Slint账户表单重新准确输入后Connect成功，实际同步合成样例prefix；Core add_account_password在保存前依次验证IMAP INBOX与SMTP认证，第一次AUTHENTICATIONFAILED来自密码键盘退格误差。SMTP发信与冷启动Asset读取已另项通过，附件回环继续验收 |
| Asset凭据冷启动与真实补同步 | 平板通过 | 修复包安装后正常启动新PID50560，随后才APPEND的UID4实际同步并触发通知权限；排除仅显示旧SQLite列表，真实IMAP认证复用Asset保存的凭据 |
| SMTP实际自发自收 | 平板/服务端通过 | 原Slint写信UI实际发送Feihong-Device-SMTP-20261011-tabletA；独立严格TLS IMAP只读核对INBOX UID5/Sent UID1的相同Message-ID、仅自身收发、中文主题正文/ZWJ family emoji，0附件。device-smtp-delivery.json |
| 原邮件回复与线程头 | 平板/服务端通过 | 原Slint回复UID4合成来信，明确将收件人替换为测试账号自身后发送；服务端INBOX UID6/Sent UID2的Message-ID相同，In-Reply-To匹配父邮件且References包含父邮件，正文中文/emoji一致。device-reply-delivery.json；自身发给自身的原邮件保留上游「无回复收件人」语义，UI线程归并另验 |
| 系统新来信通知 | 平板通过 | scope=all、UID4新增保留域From+自动邮件，首次OS授权允许后，系统通知中心实际显示唯一样例卡片及中文/emoji主题；只裁本app卡片保存matepad-actual-notification-own-card.png。此项不代替后台调度验证 |
| 测试账户 SMTP 主机连接 | 未通过 | 配置服务器465在TLS1.2/1.3认证前EOF，尚未发信；Mac DNS处于fake-IP路径，未改动系统VPN/代理/DNS，平板自身网络独立验证 |
| IMAP合成测试数据 | 主机通过 | APPEND3封仅自身样例并FETCH自己的Message-ID，中文/emoji/HTML/回复线程、两页PDF/TXT/16KiB二进制附件字节一致；此项不计SMTP收发通过，见 host-imap-fixture.json |
| JMAP/OAuth 端到端 | 待验证 | 当前配置仅覆盖 IMAP/SMTP，须另有相应测试环境 |
| CalDAV/CardDAV/WebDAV/打印/OpenPGP 实机 | 待验证 | 逐项验证真实设备与协议互通 |
| GPU 共享设备、Vulkan WSI、CPU 回退 | 待验证 | 固定 WGPU29；CPU 保持默认 |
| 实验 WGPU29 OHOS HAL 源码/ABI | 通过 | 仅独立ARM64 Vulkan cargo check与SDK/Rust结构布局断言；未接入CPU包、未创建GPU实例，不计第4阶段验收 |

完整移植完成状态须以全部阶段实际验收为准。构建失败、缺少签名或设备等情况不得写成已安装或已完成。

自动审批拒绝了向 Google DoH 查询测试服务器地址，理由是向额外服务披露DNS主机名。该查询未执行，未改用其他外部DNS绕过；设备协议验证走自身网络。

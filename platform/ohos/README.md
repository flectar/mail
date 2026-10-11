# 飞鸿邮箱 · HarmonyOS 原生移植

基于 Flectar Mail `bc0f966846a6d1758bb39ea54a24be4de6cad8d7`，保留上游 Git 历史、Rust 业务、Slint 界面、Blitz/Vello CPU 邮件渲染与 SQLite。此派生项目不是 Flectar 出品，亦未得到 Flectar 的赞助或支持。代码遵循根目录 AGPL-3.0-only；上游版权和第三方许可仍保留。

Stage/ArkTS 仅承载全窗口 XComponent、系统文件选择器、剪贴板、浏览器、通知、打印、私钥口令提示及 WorkScheduler。主包暂用用户指定的 `moe.kiwi.FlectarMail`，显示名为「飞鸿邮箱」，设备类型为 phone/tablet/2in1。发布前需确定最终标识与独立签名身份，并遵循 [上游商标政策](../../TRADEMARKS.md)。

当前为 ARM64 debug 开发版，已在一台 MatePad Pro（WEB-W00，HarmonyOS 7／API 26）验证，尚未完成三端与全部功能验收。先看 [当前状态](CURRENT-STATUS.md)，逐项结果见 [验收记录](VALIDATION.md)，性能对照见 [动画性能](ANIMATION-PERFORMANCE.md)。

## 构建与签名

本机工具基线：Rust 1.97、DevEco Studio 26、API 26 SDK，兼容 API 20 / HarmonyOS 6.0.0；ARM64 为实机目标。无需安装 Simulator。

构建主机还需 Python 3.12+、Git、Ninja、Perl 和 make，并可从 `PATH` 调用：PDFium 使用 Ninja，OpenSSL 使用 Perl／make。PDFium 7881 要求 Clang 21+；DevEco NDK 中较旧的编译器不能代替该前端要求。

下列命令从本仓库根目录运行。首次构建需要下载 Cargo、OHPM、OpenSSL 及固定版本的 PDFium 源码依赖；Rust 使用 OHOS workspace 的锁文件并执行 `cargo build --locked`。PDFium 7881 另需 Clang 21 或更新版本，可给脚本传 `--cxx /path/to/clang++`；最终使用 OHOS sysroot 和 NDK 链接器，不使用 macOS／Android ABI。

按下方命令使用 PDFium 默认工作目录 `target/pdfium-worktree`。脚本虽支持 `--work-dir`，当前 Debug HAP 的 PDFium 探针头文件仍固定从 `target/pdfium-worktree/pdfium` 读取；只更改脚本工作目录不足以构建该探针，因此本文复现步骤不指定自定义目录。

```sh
rustup target add aarch64-unknown-linux-ohos
python3 scripts/build-ohos-pdfium.py
python3 scripts/ohos-build.py --debug --sign
```

签名步骤须先配置下述专属身份。没有签名时可去掉 `--sign` 构建 unsigned HAP；它不代表可安装华为实机。默认构建 release；本轮完整设备验证使用的是 `--debug`，不能将 debug 结果直接作为 release 验收。

`--rust-only` 只编 Rust，`--arkts-only` 只编 Stage；`--hap-only` 使用已有的对应模式 Rust 静态库打包。修改 Rust／Slint／依赖后需先重新构建，不能只打包旧库。脚本将缓存写入 `target` 与 `platform/ohos/.cache`。`DEVECO_STUDIO_CONTENTS` 指向 DevEco 的 Contents 目录，`OHOS_SDK` 指向其 `openharmony/native` 目录，可用于覆盖本机默认位置。

OHOS debug profile 保留断言、调试符号和调试入口，为采样命中的 Slint、Blitz/Vello CPU、SVG、字体及布局依赖配置优化，原应用及平台桥为2；C++ debug 桥使用 `-O2`。具体覆盖以 [OHOS Cargo 配置](rust/Cargo.toml)和实际 rustc 命令为准，不能假定同名依赖的所有版本均已优化。实际编译选项与真机帧间隔分别核对。

未配置上游可选 `.env` 时，构建脚本使用 `target/ohos/oauth-build.env` 这个稳定的空文件；以后添加 `.env` 会自动重新选择它。可通过 `FLECTAR_OAUTH_BUILD_ENV_FILE` 明确指定构建注册配置路径。运行时仍支持用户填写自己的 OAuth 注册配置。Slint 的生成品牌目录不作为自身构建输入，实际 UI、翻译、资源及编译环境的变更仍会触发重建。

在 DevEco 中为此 bundle 创建自己的开发签名，并将当前真机加入授权设备；保留本机证书、profile 和密钥文件。把该工程的 `signingConfigs` 数组及对应 `products` 配置保存到 `platform/ohos/.signing/signing-configs.json`，文件使用 JSON 格式，产品名为 `default`，权限设为 `0600`。配置里的证书和密钥路径应指向本机文件；不要提交此文件、私钥、密码或设备授权标识。构建脚本要求配置含非空 `signingConfigs`，并将产品的 `signingConfig` 临时合并到共享 build-profile，构建完成或失败后恢复原字节与权限。SDK 公开 OpenHarmony 测试证书不能代替真机授权配置。

签名 HAP 输出为 `platform/ohos/entry/build/default/outputs/default/entry-default-signed.hap`；unsigned 对应 `entry-default-unsigned.hap`。脚本会检查包内 `libfeihong.so`、`libpdfium.so` 是 AArch64 ELF，并检查 PDFium 许可。PDFium 输出的许可清单与编入库内的 ICU 数据无需另行寻找。以后只修改 ArkTS／包装或重新构建 PDFium，可使用已更新的 Rust 库打包：

```sh
python3 scripts/ohos-build.py --hap-only --debug --sign
```

## 安装与正常启动

设备需开启 USB 调试、授权这台开发主机并解锁。使用 SDK 的 HDC，先确认目标，再安装自己的签名包。命令中的路径和设备选择器需按本机替换：

```sh
FEIHONG_HDC="/path/to/sdk/hdc"
"$FEIHONG_HDC" list targets
FEIHONG_DEVICE="your-device-id"
"$FEIHONG_HDC" -t "$FEIHONG_DEVICE" install -r \
  platform/ohos/entry/build/default/outputs/default/entry-default-signed.hap
"$FEIHONG_HDC" -t "$FEIHONG_DEVICE" shell \
  aa start -a EntryAbility -b moe.kiwi.FlectarMail -m entry
```

普通启动使用应用私有数据与 Asset 凭据，默认关闭性能诊断和强制亮屏。

## 不连接邮箱的界面调试

签名 debug 包支持单独的离线界面配置，使用原有 SQLite、邮件列表、Blitz 邮件正文和 PDF 附件界面。样例含中文、emoji、回复线程、复杂 HTML 与两页 PDF。它使用独立的 `filesDir/debug-ui-fixture` 数据和缓存目录，不读取正常账户凭据，并暂停协议同步、发件和后台通知。release 包不启用此入口。

先停止应用，再通过 Want 参数冷启动；运行中的配置不能切换：

```sh
"$FEIHONG_HDC" -t "$FEIHONG_DEVICE" shell aa force-stop moe.kiwi.FlectarMail
"$FEIHONG_HDC" -t "$FEIHONG_DEVICE" shell \
  aa start -a EntryAbility -b moe.kiwi.FlectarMail -m entry --ps feihongUiFixture true
```

调试配置在应用前台保持本应用窗口亮屏，进入后台时释放此设置。只停止应用并以普通入口重启，即可回到正常账户配置。`--ps feihongPdfiumSmoke true` 可在 debug 冷启动时额外执行 PDFium 创建、保存、重新打开、文本与像素渲染探针；此探针不代表附件界面验收已通过。

普通真实账户调试时，可给 debug 包添加 `--ps feihongKeepScreenOn true`，仅当前应用窗口在前台保持亮屏；后台或窗口关闭会清除。此标志不切换样例配置、不暂停协议，release 包忽略。已启动的 debug 应用可用同一 `aa start` 传入该参数，通过 `onNewWant` 生效；传入 `false` 关闭。

性能诊断默认关闭。debug 冷启动时添加 `--ps feihongPerformance true`，可每2秒汇总事件处理、计时器、Slint绘制、Native提交路径的耗时与成功提交数；原生层另记录buffer请求、fence与Map/复制/提交耗时。日志只含数字，不含邮件或输入文本。提交成功不等于显示器已经呈现。当前检测脚本的schema 2使用RS第二列 `presentTime`，第一列 `flushTime` 另存于 `flush_stats`；此语义依据公开Rosen master，尚未匹配设备RenderService版本。可重复步骤及版本限制见 [性能说明](PERFORMANCE.md) 与 [测量口径](MEASUREMENT-SCHEMA.md)。静止屏幕不会被当作低帧率错误。

设置、侧栏与邮件切换的动画长帧须单独检测，不能用短时间拖动的平均FPS代表。该轮定位、修复与实机对照见 [动画性能](ANIMATION-PERFORMANCE.md)；检测脚本另记录超过33.33／50／100ms的间隔数量及连续次数。

第四轮 CPU 包已安装验证；首次邮件阻塞明显减轻，短 HTML 仍有约58ms候选间隔，未宣称完全无卡顿。最新版包摘要、冷／暖态范围及剩余验收项集中在 [当前状态](CURRENT-STATUS.md)，避免用历史包或主机计时代表当前设备结果。

`scripts/ohos-perf.py` 可在明确执行连续手势、schema 2第二列有效且采样覆盖充分时按所选FPS阈值给出统计评估；设备版本与动作归属仍须核验，平均值不能证明短动作流畅。静止、覆盖不足和环形记录可能截断时不作性能失败判定。新输出须使用新文件，不覆盖历史报告。后端的FIFO、像素和帧调度回归可以通过 [主机契约测试](tests/README.md) 重跑，包含旧帧调度顺序的负对照。主机模拟回调不替代实机测量。

历史 schema 1 的第一列数字保留为提交统计，以其宣称实际呈现 FPS 提升或30 FPS流畅通过的结论已经撤回；独立复制计时、搜索、键盘重开、窗口／后台恢复和像素验证继续保留。历史更正见 [性能说明](PERFORMANCE.md)与 [验收记录](VALIDATION.md)。

## 测试与许可记录

轻量自测不需要设备或测试账户：

```sh
python3 scripts/ohos-perf.py --self-test
```

生产 owner loop、输入及帧缓冲契约使用锁定 Slint／i-slint-core 1.18.1 的主机依赖，准备与执行命令见 [测试指南](tests/README.md)。设置首次构建和字体预热各有 [设置状态](tests/SETTINGS-LAZY-CONTRACTS.md)与 [字体缓存](tests/FONT-PREWARM-CONTRACTS.md)契约，包含实际源码运行和应失败的负对照。ARM64 metadata／syntax 检查不计设备运行通过。

构建自动运行 [第三方许可收集脚本](../../scripts/ohos-licenses.py)，PDFium 构建另收集其许可；生成资源、编译缓存与原始 `target/ohos-evidence/`、`target/ohos-host-contracts/` 记录不作为 Git 源码文件提交。许可文本随 HAP 打包，源许可及脚本保留；重要验收摘要已留在本目录文档中。复测使用新输出文件，不覆盖历史记录，不保存账户密码或邮件输入文本。产品品牌由 [飞鸿资产](branding/feihong.svg)及 OHOS 构建 overlay 提供；根 README 的原上游内容继续用于来源说明。

## 运行与系统边界

- 一个 Rust 界面线程拥有所有 Slint 对象；Native 输入、生命周期和 Tokio 工作通过队列进入该线程。Surface 按 generation 验证，防止重建后提交旧帧。
- CPU SoftwareRenderer 与 Blitz/Vello CPU 为默认路径。NativeWindow 提交等待 acquire fence、按真实 stride 写像素并归还 buffer。GPU 为独立可选阶段，未完成验证前不改变默认值。
- SoftwareRenderer 保留不透明 RGBA 帧缓冲；NativeWindow 按缓冲序号累计各自错过的变化，按 stride 复制待补齐区域，成功提交后才确认该缓冲已经补齐。原生提交失败时保留待补齐区域；后台释放缓存，恢复后从持久帧缓冲整屏补齐。未知缓冲、Surface重建和尺寸变化也从整屏开始。Rust 界面线程在前台使用系统 `QOS_USER_INTERACTIVE`，后台／暂停／事件循环退出恢复默认调度；同步 worker 保持各自的调度。QoS API 失败不会阻止绘制，诊断可记录实际返回值和等级。
- 中文字体通过系统字体描述及明确路径登记，同时提供给 Slint、Blitz/Parley 和编辑器字体库。中文预编辑、选区及 emoji 在 UTF-16 与 UTF-8 边界转换。
- 数据位于应用 `filesDir/data`，缓存位于 `cacheDir`。凭据与私钥只在 Asset Store；长值分为最多 1,000 字节的短项，并先写新 generation、再更新主 manifest。
- 导入 URI 由系统提供方复制到私有 staging，Rust 只读私有路径。取消和未消费结果释放 staging；导出完成文件后才打开保存选择器。
- OAuth 复用状态校验、PKCE 与 loopback broker；注册配置仍可由用户填写，不能在后台弹出交互登录。受系统调度的同步有时间预算，前台恢复时补同步。后台门控阻止新的同步/发件任务；已经提交的 SMTP 和当前协议周期在安全边界自然完成，不宣称立即断网。
- 打印由同一 Blitz/Vello CPU 管线逐页生成有上限的 raster PDF，再交给 PrintKit。PDF 附件预览保留独立 PDFium 工作线程及取消/容量限制。

## 当前验收

以 [当前状态](CURRENT-STATUS.md)和 [验收记录](VALIDATION.md)中的实际执行结果为准。代码实现、交叉编译、签名、安装和端到端验收是不同状态；缺少设备或账户时不标记阶段完成。

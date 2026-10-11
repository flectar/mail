# 飞鸿邮箱性能诊断

2026-10-11第四轮动画修复包已安装到MatePad。补齐实际采样命中的SVG／字体依赖优化、CPU大面积动画及设置构建之后，本轮再将首次邮件字体初始化提前到后台。三次进程冷启动首候选等待由154–163ms降至56–94ms，事件窗口峰值由216–226ms降至53–55ms。暖态六场单轮中，设置／侧栏／回复最大候选间隔17–42ms，HTML仍有一次58ms间隔，不能判定所有交互完全无卡顿。完整双列、按下序列及全部慢间隔保留；该轮优化到此结束。具体实现、包身份及实机回测见 [动画性能](ANIMATION-PERFORMANCE.md)，全功能与设备范围见 [当前状态](CURRENT-STATUS.md)。

本文同时保留缓冲损伤修复包等早期结果，历史段落中的包并非最新版。原始匿名采集与主机输出位于本地忽略目录 `target/ohos-evidence/`、`target/ohos-host-contracts/`，不随Git提交；文中的文件名是本地索引。外部复测须重新采集并生成新文件，本页及动画性能记录已保留关键结果与验收边界。

**时间戳口径更正：** 旧 `schema_version=1` 脚本读取 RS 导出的第一列，却将结果命名为 `actual_present_fps`。[公开 Rosen 源码的 `RSSurfaceFps::Dump`](https://github.com/openharmony/graphic_graphic_2d/blob/master/rosen/modules/render_service_base/src/gfx/fps_info/rs_surface_fps.cpp) 将每槽输出为 `flushTime:presentTime`。因此下文保留的旧数值应解释为第一列的**提交时间统计**，不能作为实际呈现 FPS；原报告和历史数字不重写，由它们得出的实际呈现提升及短动作流畅通过结论撤回。复制耗时、主机契约、搜索、窗口／后台恢复和像素对照等独立证据仍有效。

当前脚本使用 schema 2，按第二列 `presentTime` 计算呈现区间统计，同时将第一列完整保留于 `flush_stats`，接口详见 [测量口径](MEASUREMENT-SCHEMA.md)。这里的字段语义依据公开 master，尚未匹配 MatePad 所安装 RenderService 的源码版本；输出明确保留 `device_source_version_verified=false`。最终设备呈现验收还需核对该版本与实际动作边界。Rust 和 Native 分段计时用于定位开销，成功提交不等于已经显示；不能把绘制耗时的倒数当作实际 FPS。静止界面没有持续重绘时，不按低 FPS 判定异常。

## 已定位的瓶颈与修复

原始 debug 包在 MatePad 的 2880×1786 窗口上已观察到严重卡顿；旧第一列统计约 3.4 次提交／秒。自身进程 CPU 采样、与签名包 `.text` 完全相同的符号，以及原生分段计时共同确认：主要开销在 Slint 的 item 遍历、生成 UI 分派和软件绘制；Native 提交约 26–31 ms，不能解释约 297 ms 的提交间隔。

OHOS 独立 workspace 对生成应用、后端和绘制依赖分别启用优化，保留 debug 断言。SoftwareRenderer 使用持久的不透明 RGBA 帧缓冲，免去旧的 RGB→RGBA 逐像素扩展。事件队列每轮最多处理 128 项或 4 ms，然后运行计时器和绘制；事件继续按 FIFO 保留。VSync 在 CPU 绘制开始前预约，允许绘制期间到达的刷新信号排队，避免绘制结束后再增加一轮等待。UI 线程前台设置交互 QoS，后台与退出重置；QoS 的性能作用须单独测量。

Native按缓冲序号和实际配置保留最多8条待补齐区域记录。每次Slint修改持久帧缓冲，先将变化并入所有已知Native缓冲；本次取得的缓冲只复制它累计错过的区域，并保留行尾padding。只有Unmap和Flush均成功才确认该缓冲已经补齐；映射或提交失败时保留待补齐区域。后台会释放整个缓存，恢复后未知缓冲从持久帧缓冲整屏补齐。未知、淘汰、配置改变及新Surface的缓冲也从整屏复制开始。Rust在Surface generation、物理宽高或密度改变时强制完整绘制，含像素数不变的宽高互换。仍向合成器提交完整Region，避免将缓冲轮转的损伤误当作显示帧间的损伤。

缓存条目持有NativeBuffer的公开对象引用，使对应对象在追踪期间保持存活，避免已销毁对象的序号复用误命中。淘汰、尺寸变化、Surface重建及进入后台会释放这些引用；引用获取失败则整屏复制且不缓存该缓冲。正常缓冲池已有的对象只增加引用；被系统替换的旧缓冲可能延长存活，数量最多8个，实际内存占用须单独测量。

实际设备对照必须保留相同邮件、缩放、窗口和手势。多次 HDC 手势之间有空窗，完整记录的区间统计包含这些间隔；另列的持续拖动样本只能代表其时间戳覆盖区间。历史第一列统计不能改称呈现 FPS。新第二列统计也不能将短区间扩大到整段请求时长，不能由 CPU 绘制耗时推算 60 FPS 已达标。

额外隔离了空主题输入框的光标重绘：整屏复制墙钟约97–100 ms，线程CPU时间约97–99 ms；同包HTML滚动复制的两种计时均为1.36 ms。两场景的窗口、stride11520及实际bufferUsage1547相同。线程CPU计时确认该段主要消耗在线程运行时，尚不能确定缓存、频率或带宽的具体原因。增加READ_OFTEN提示的单项对照未改善，已撤回。缓冲区损伤累计修复后，匹配操作的像素复制均值从93.26 ms降至4.07 ms（包含首次暖机），稳态为2.29 ms；完整对照见下文。

`scripts/ohos-perf.py` 只依赖 Python 标准库。离线模式不会调用 HDC。实时模式需要明确指定设备和当前应用 PID，不安装或启动应用，不改系统设置，不开启全局 trace。输出为匿名数字 JSON，文件权限为 `0600`；不输出或保存原始日志、设备地址、邮件内容和账户信息。

## 离线复核

从仓库根目录运行：

```sh
python3 scripts/ohos-perf.py \
  --fps-file target/ohos-evidence/matepad-perf-baseline-fps.txt \
  --output target/ohos-evidence/perf-baseline-analysis-schema2.json

python3 scripts/ohos-perf.py \
  --fps-file target/ohos-evidence/matepad-perf-native-probe-fps.txt \
  --output target/ohos-evidence/perf-native-probe-analysis-schema2.json

python3 scripts/ohos-perf.py --self-test
```

输出必须使用新的文件名，脚本拒绝覆盖已有文件。以上命令创建 schema 2 分析，不替换原 schema 1 报告。输入必须是本应用 Surface 的 RS 导出，包含 `feihong-surfaceSurface` 标记及 `flushTime:presentTime` 纳秒时间戳槽。脚本保留原环形槽与两列完整时间戳，忽略 `0:0` 空槽，分别排序两列且保留所有有效长间隔。第二列区间统计的公式为：

```text
frame_count = 有效时间戳数 N
span_seconds = (最后 presentTime - 最初 presentTime) / 1e9
actual_present_fps = (N - 1) / span_seconds

flush_stats.span_seconds = (最后 flushTime - 最初 flushTime) / 1e9
flush_stats.observed_flush_fps = (N - 1) / flush_stats.span_seconds
```

主结果为 `timestamp_column=second_present`；`flush_stats.timestamp_column=first_flush`。输出另含全部原始槽、排序后时间戳、逐间隔及 median、p95、max（毫秒），p95 使用 nearest rank。少于两个有效样本、无效时间戳、重复时间戳或缺少本应用 Surface 标记时，相应列输出 `status=unavailable`、具体原因及空速率；主呈现统计不可用时进程退出码为 `2`。不能由日志推断手势是否连续；静止窗口也不会补造帧率。第二列语义仍受前述设备源码版本限制。

2026-10-11 MatePad 旧 schema 1 离线结果如下。数字原样保留，全部来自第一列，是提交时间基线，不能据此宣称实际呈现 FPS 或优化已经生效。

| 历史记录 | 有效提交槽数 | 提交时间跨度 | 提交次数／秒 | 提交间隔 median | 提交间隔 p95 | 提交间隔 max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `matepad-perf-baseline-fps.txt` | 35 | 10.015 s | 3.395 | 296.969 ms | 467.930 ms | 483.270 ms |
| `matepad-perf-native-probe-fps.txt` | 34 | 9.428 s | 3.500 | 293.435 ms | 488.717 ms | 508.774 ms |

随后相同邮件、100% 缩放、2880×1786、CPU 模式的真实安装包旧第一列对照如下。固定八次手势有命令间空窗，长手势的提交时间区间分别只覆盖约 1.8 秒，故分开列出；这些数字不构成实际呈现 FPS 或流畅验收。

| 历史安装包／操作 | 提交时间区间 | 提交次数／秒 | 提交间隔 median | 提交间隔 p95 |
| --- | ---: | ---: | ---: | ---: |
| 首轮绘制优化／八次手势 | 9.158 s | 11.247 | 36.188 ms | 423.511 ms |
| 加UI线程QoS／八次手势 | 8.831 s | 11.437 | 37.044 ms | 399.147 ms |
| 提前预约VSync／八次手势 | 8.996 s | 16.674 | 21.393 ms | 415.167 ms |
| 最终缓冲损伤修复／八次手势 | 8.966 s | 18.290 | 19.170 ms | 336.482 ms |
| 首轮绘制优化／长手势动态段 | 1.877 s | 27.698 | 33.238 ms | 50.908 ms |
| 加UI线程QoS／长手势动态段 | 1.775 s | 28.738 | 33.335 ms | 47.396 ms |
| 提前预约VSync／长手势动态段 | 1.789 s | 43.048 | 20.930 ms | 28.688 ms |
| 最终缓冲损伤修复／长手势动态段 | 1.804 s | 46.013 | 19.016 ms | 25.846 ms |

匿名原始数字分别保存在 `target/ohos-evidence/matepad-perf-{optimized,qos,pacing}-{10s,5s}.json`。提前预约 VSync 的实际后端主机契约覆盖回调在呈现期间排队、快帧等待、失败回退和 Surface generation 替换；旧提交后预约的负对照按预期失败。主机模拟回调用于回归，不替代实机 FPS。

## 缓冲损伤修复包的历史平板结果

2026-10-11，缓冲损伤修复历史包已签名并安装到同一MatePad Pro。包文件为 `target/ohos-evidence/perf-damage-signed.hap`，SHA-256为 `a9e2cf2119ac7f7aa3103ea20097e3ed520ca8728650c5b87a4b8b3eb659a5ad`。构建与生产源摘要记录于 `perf-damage-build.json`；当前第四包身份另见动画性能记录。

相同HTML邮件、100%缩放、2880×1786、同八次HDC手势，旧第一列区间统计从3.395变为18.290次提交／秒，约5.39倍；这不是已确认的呈现 FPS 提升。历史最大提交间隔805.624 ms仍保留，命令间空窗计入均值和p95，没有删除长间隔。两次长手势的提交区间统计为46.013次／秒，跨度只有1.804秒，不能代表请求5秒或实际呈现。原证据 `matepad-perf-damage-{10s,5s}.json` 不变。

旧实时检测脚本还做过4次2秒连续手势采集，并错误地将第一列统计用于30 FPS阈值。原报告的两次 `pass` 和两次 `inconclusive` 原样保留，但由前两次推断实际呈现或短动作流畅通过的结论撤回：

| 历史检测记录 | 有效提交槽数 | 提交时间跨度 | 提交次数／秒 | 旧覆盖率 | 原报告状态／当前口径 |
| --- | ---: | ---: | ---: | ---: | --- |
| `matepad-perf-damage-detector-1.json` | 110 | 2.184 s | 49.897 | 109.22% | 原pass；呈现验收撤回 |
| `matepad-perf-damage-detector-2.json` | 1 | 不足两个时间戳 | 不可用 | 不可用 | inconclusive |
| `matepad-perf-damage-detector-3.json` | 112 | 2.212 s | 50.180 | 110.60% | 原pass；呈现验收撤回 |
| `matepad-perf-damage-detector-4.json` | 44 | 1.173 s | 36.649 | 58.67% | inconclusive |

第2次可能触及工具栏而未持续滚动，动作说明更正独立保存于 `perf-damage-detector-2-context-correction.json`，没有修改原报告；第4次原脚本因覆盖不足而不判通过。旧覆盖率也来自提交时间跨度，不能当作实际动作或呈现覆盖。上述短记录不能证明当前场景达到30 FPS呈现基准；需用第二列、匹配的设备语义和可信动作边界重新验证。

光标对照使用同一空主题、相同5段操作与等待时间，计时期间不截图或OCR。各两秒窗口按成功提交数加权，以下均值来自保留两位小数的匿名日志：

| 光标场景 | 窗口数／成功提交数 | 像素复制墙钟均值 | 像素复制线程CPU均值 | Native提交路径均值 |
| --- | ---: | ---: | ---: | ---: |
| 修复前测量窗口 | 11／47 | 93.26 ms | 92.86 ms | 104.22 ms |
| 修复后，包含首次暖机 | 11／51 | 4.07 ms | 4.04 ms | 15.05 ms |
| 修复后稳态，单独列出 | 10／47 | 2.29 ms | 2.26 ms | 13.12 ms |

首次暖机复制均值25.10 ms包含在修复后总均值内。进入输入框前的HTML／焦点结转窗口另作分类，两侧全部窗口都保存在原文件，没有以其代表空主题光标。尺寸、stride11520、usage1547及CPU时钟有效样本数核对一致。证据为 `perf-copy-cpu-probe-ime-isolated.json`、`perf-damage-ime-isolated.json` 与独立数字复核 `perf-independent-damage-metric-audit.json`。光标按其正常闪烁频率呈现，不用该场景的低FPS判定卡顿。

搜索输入／清除、Back隐藏输入法及再次打开均通过。真实悬浮窗1008×1714（generation2）恢复全屏2880×1786（generation3），同进程完成完整重绘；Home后恢复也通过。原HTML指定裁剪区域696,500像素在修复前后及窗口／后台恢复后均无差异，此结论限于该区域。这里观察到的是窗口尺寸与generation变化，未将它记为已观察到SurfaceDestroyed+SurfaceCreated。

同HTML静止时应用RSS约520MB，前一复制探针包约519MB，最终长手势时约523MB；该采样不覆盖合成器全部缓冲分配，也不证明峰值内存或耗电达标。最终包已不带性能参数冷启动，实际重绘后未产生性能汇总，确认诊断默认关闭；原账户保留，手动同步显示完成。汇总见 `target/ohos-evidence/perf-damage-final-validation.json`，其中旧呈现FPS／阈值通过口径按本文撤回，独立功能与像素结果保留；分项证据为 `perf-damage-{floating-surface,restored-full-surface,background-resume,default-off,surface-background-pixel-comparison}.json`。该轮生产Rust23项、两组应失败的负对照、C++13项及ASan/UBSan、旧检测脚本31项执行通过；检测用例通过不能纠正旧第一列的误命名。运行方式见 [主机契约测试](tests/README.md)。

## 实时采集

由当前设备调试负责人执行，避免与安装、冷启动、输入或其他采集任务重叠。使用包含调试诊断的应用包，先冷启动并传入 Stage 参数 `--ps feihongPerformance true`；该参数默认关闭，Rust 诊断汇总约每两秒记录一次。

```sh
"$FEIHONG_HDC" -t "$FEIHONG_DEVICE" shell \
  "aa force-stop moe.kiwi.FlectarMail"
"$FEIHONG_HDC" -t "$FEIHONG_DEVICE" shell \
  "aa start -a EntryAbility -b moe.kiwi.FlectarMail -m entry --ps feihongPerformance true"
```

`FEIHONG_HDC` 是本机 HDC 可执行文件路径，`FEIHONG_DEVICE` 是负责人选定的设备选择器，`FEIHONG_PID` 是本次冷启动后的应用 PID。已运行的旧进程不能用旧 PID 或旧启动参数冒充新的诊断会话。

保持同一邮件、窗口尺寸、密度和渲染模式，在采集期间做连续且可重复的同一组滑动操作：

```sh
python3 scripts/ohos-perf.py \
  --device "$FEIHONG_DEVICE" \
  --app-pid "$FEIHONG_PID" \
  --hdc "$FEIHONG_HDC" \
  --duration 5 \
  --output target/ohos-evidence/perf-live-schema2.json
```

默认时长是 10 秒，可指定 1–60 秒。脚本先读取限定 PID/tag 的日志快照，仅清空本应用 Surface 的 FPS 计数，再等待指定时长并读取本应用 RS 时间戳和新增的匿名计时汇总。它不会清空系统日志或其他 Surface 的计数。`fpsClear` 只是采集操作，不能据此证明所有槽均属于随后动作，也不能从空槽或长间隔自动确定动画开始／结束。

本设备历史 RS 导出包含384个环形槽，脚本按实际输入保留槽数。高 FPS 时，10 秒记录可能只保留最近一段。报告始终使用所选列的 `span_seconds`，不把请求采集时长作为帧率分母；所有导出槽均非空时，标记 `ring_buffer_truncated_potential=true`。这表示可能覆盖不全，不能把槽数称为整个请求时长的总呈现数。高刷新率设备可缩短至 2–3 秒，并保留每次实际覆盖的跨度。

实时采集目前使用以下命令，其语法已按本机连接设备的官方 help 核对：

```sh
"$FEIHONG_HDC" -t "$FEIHONG_DEVICE" shell \
  "hidumper -s RenderService -a 'feihong-surfaceSurface fpsClear'"
"$FEIHONG_HDC" -t "$FEIHONG_DEVICE" shell \
  "hidumper -s RenderService -a 'feihong-surfaceSurface fps'"
"$FEIHONG_HDC" -t "$FEIHONG_DEVICE" shell \
  "hilog -x -P $FEIHONG_PID -T FeihongPerf,Feihong"
```

`-a` 参数必须保留整个引用字符串。Native 日志的 tag 是 `FeihongPerf`；Rust 通过桥接记录的 tag 是 `Feihong`，消息以 `FeihongPerf [feihong-perf]` 开头，所以必须读取两者。脚本还在本机内存中验证 PID、应用 bundle（若 header 含 bundle）和完整数字字段白名单，只保存支持的匿名汇总。未知日志不写入报告。没有新增支持的日志时，`perf_log_status` 明确标记缺失，不能等同于计时为零。

日志按完整 header（含时间戳）与数字消息去重，并忽略排版空格差异。采集前已存在的记录和采集后重复输出的同一记录不会计为新窗口；时间戳不同但数字相同的真实新窗口仍保留。新增的两秒计时汇总可能跨越采集边界，不能把其窗口与 RS 时间戳覆盖区间视为完全相同。

本次官方 help 保存于本机忽略目录 `target/ohos-evidence/hdc-render-service-help.txt`、`hdc-hidumper-help.txt` 和 `hdc-hilog-help.txt`。换 SDK 或设备时，由负责人复核 `hidumper -s RenderService -a '-h'`、`hidumper -h` 和 `hilog -h`，不得猜测替代命令或扩展采集范围。

## 如何解释分段计时

- Rust 汇总包含事件队列批次与事件数、timer/animation、Slint render、Native present、完整 frame work，以及成功提交间隔的样本数、p50、p95、max。这里的 `present` 名称是程序内提交路径，未等待或证明显示器实际呈现。每种分布保留最近 512 个样本计算百分位，count/total/max 覆盖整段窗口。
- `active_fps` 是显式活跃测量期间的成功提交数除以真实 `active_span`，包括等待和慢帧；`wall_fps` 使用整段墙钟时间。二者不能等同于实际呈现 FPS。后端 active 标记是辅助定位信息；最终手势验收需匹配设备的 RS 第二列语义及记录动作上下文。
- 静止且没有帧尝试的窗口输出 `idle=true`，FPS 为 `none`。采集文件没有两个有效第二列时间戳时，主 RS 分析输出 `unavailable`，不能仅凭这个结论宣称应用卡死。
- Native 记录 request、fence、map/copy、flush 和完整提交路径延迟。旧探针字段 `copyMs` 与新字段 `mapCopyMs` 均包含缓冲配置读取、映射和当次像素复制；旧包复制整帧，最终包复制该缓冲待补齐的区域。不能把合计全部归因于像素转换。
- UI 线程 QoS 记录为 `kind=ui_qos`，只保留 `interactive`、`setStatus`、`beforeStatus`、`beforeLevel`、`afterStatus`、`afterLevel` 六个整数。`interactive=1` 表示请求前台交互级别，`0` 表示重置；`setStatus` 是设置或重置调用的实际返回状态。Get 失败时，级别 `-1` 表示未知，重置成功后 Get 也可能返回 `-1`，不得据此猜测实际级别或设置成功。这些记录沿用自身 PID/bundle/header 过滤和时间戳去重。QoS 实验只作用于 Rust UI owner 线程，前台设置、后台重置、退出时由 guard 重置；其效果仍须用相同操作的 RS 时间戳验证。
- 对照时记录应用构建、CPU/GPU 模式、窗口实际像素尺寸、操作和实测时间跨度。只比较相同条件下的连续操作记录；静止、切换邮件与等待网络的窗口另作上下文说明。

脚本边界测试覆盖环形排序、空槽、不足样本、重复/无效时间戳、长间隔、日志 PID/bundle/tag 过滤、两种 Native 字段、Rust idle 汇总、旧日志快照去重，以及 QoS 设置/重置/Get 未知/设置错误与过滤。当前真实 Native 探针文件的 10 条匿名日志均能解析。实时命令由设备负责人验证后，才能作为对应应用构建的实机性能证据。

## 可选连续手势评估

schema 2 更正了默认统计列，输出不再与旧 schema 1 口径相同。需要重复检测开发基准时，由操作员明确确认会在整个采集窗口持续滑动，再添加 `--continuous-gesture`；下列评估仍受设备源码版本未匹配的限制：

```sh
python3 scripts/ohos-perf.py \
  --device "$FEIHONG_DEVICE" \
  --app-pid "$FEIHONG_PID" \
  --hdc "$FEIHONG_HDC" \
  --duration 2 \
  --continuous-gesture \
  --min-fps 30 \
  --output target/ohos-evidence/perf-continuous-schema2-1.json
```

对同一邮件、同一尺寸和同一渲染模式重复三次，每次使用不同输出文件。操作员须持续滑动至采集结束；等待网络、切换邮件、手势间暂停或静止界面时，不应确认该标志。`operator_asserted_continuous_gesture` 只记录操作员声明，脚本不会从日志自动认定真实手势连续。

`--min-fps` 是开发者选择的回归基准。启用连续手势评估且未指定阈值时，默认是 30 FPS，可按测试目标调整。该基准不代表设备能力上限，也不代表 60 FPS 或完整移植验收已经通过。单独指定 `--min-fps` 而未确认连续手势，结果为 `not_assessed`。

评估只接受 schema 2 的 `timestamp_column=second_present` 与 `actual_present_fps`，不删除慢帧、不更换分母。有效第二列时间戳跨度须至少覆盖请求时长的80%，且环形槽未满。5秒测量需要跨度至少4秒；短暂高帧率片段不能代表整个测量窗。只给出最近一段的满环形缓冲也不会判定整个窗口通过。覆盖率本身不证明槽与动作的归属；脚本阈值通过也不解除设备语义版本限制。

| `assessment.status` | 含义 | 退出码 |
| --- | --- | ---: |
| `pass` | schema 2第二列有效，明确确认连续手势，覆盖充分且未截断，观测速率达到阈值；设备版本和动作归属仍需核验 | 0 |
| `fail` | 同样满足脚本测量条件，观测第二列速率低于阈值 | 1 |
| `inconclusive` | schema／列不匹配、FPS不可用、缺请求时长、覆盖不足或环形缓冲可能截断 | 2 |
| `not_assessed` | 操作员未确认连续手势，保留观测报告 | 原有规则：有效数据时退出 0，不可用时退出 2 |

`coverage_ratio`、`required_coverage_ratio` 和 `threshold_applied` 在评估中明确记录。纯 RS 文本导出没有请求时长元数据，离线模式即使确认手势也会给出 `missing_capture_duration`，不会猜测采集时长。离线双列统计仍可使用；历史 schema 1 的 `actual_present_fps` 不能直接带入新评估。

检查每次报告的列、跨度、逐间隔、p95/max和分段耗时。平均速率阈值不能证明短动作流畅，卡顿分布、画面正确性和稳定性继续分别核验。

## 单动作的可信边界

`--input-monotonic-ns` 只接受本次采集、对应应用PID的真实 `Native input boundary` 数字。只有独立确认 RS 时间戳与该输入记录共享 `CLOCK_MONOTONIC` 时，才能添加 `--confirm-clock-monotonic`；不能仅由数值接近、`fpsClear`、空白快照或长间隔猜测。

```sh
python3 scripts/ohos-perf.py \
  --fps-file target/ohos-evidence/action-rs-dump.txt \
  --input-monotonic-ns "$FEIHONG_INPUT_MONOTONIC_NS" \
  --confirm-clock-monotonic \
  --output target/ohos-evidence/perf-action-schema2.json
```

此命令仅适用于已经取得且核验上述真实边界的记录。`action_fps` 独立保留 `all_fps`、`candidate_fps`、`candidate_pairs_ns` 与 `unattributed_pairs_ns`。确认共同时间基准后，仅把 `flushTime >= input_monotonic_ns` 的有效槽列为候选，再用候选第二列计算区间；全部原槽、全部间隔与原环形截断状态仍保留。没有可信输入或时钟确认时为 `inconclusive`，不会自动裁剪旧帧。候选仅有输入后的提交下界，不证明动作结束、连续动画或每一槽的动作归属，不能据此给短动作流畅盖章。

## Native 缓冲细分计时

新诊断包在 `kind=native_present` 中保留旧合计字段，并追加四个完整的匿名毫秒字段：

| 字段 | 测量阶段 |
| --- | --- |
| `configMs` | 读取 NativeBuffer 配置 |
| `mapMs` | 映射 NativeBuffer |
| `pixelCopyMs` | 写入当次待补齐的像素；旧实现始终复制整帧 |
| `unmapMs` | 解除 NativeBuffer 映射 |

`mapCopyMs` 继续表示配置读取、映射和像素复制三段的合计；`flushMs` 继续包含解除映射与提交 NativeWindow 的耗时。增加细分字段不会改变原有数值含义。日志各字段独立保留两位小数，细分值相加与合计可能存在舍入差异；脚本原样保留数值，不用相加结果替换合计。

白名单兼容旧 `copyMs` 或 `mapCopyMs` 格式。四个细分字段必须同时出现或全部缺省，未知字段、部分细分和非数字值均不作为支持的 Native 汇总。旧记录没有细分信息时，应标记缺少测量，不能把缺省阶段当作零耗时。

细分用于区分实际时间花在哪个调用或复制段。比较光标重绘与滚动时，仍需保持构建、尺寸、帧数和场景上下文；较高的映射或复制耗时本身不说明产生它的原因，原因须用独立对照验证。

后续诊断包在完整四项细分后追加另一组四个字段，旧包格式仍受支持：

| 字段 | 含义 |
| --- | --- |
| `pixelCopyCpuSamples` | 像素复制段成功取得线程 CPU 时钟的有效样本数 |
| `pixelCopyCpuMs` | 有效样本的线程 CPU 毫秒均值；无有效样本时为 `none` |
| `strideBytes` | 窗口内最后一个缓冲样本的 stride，单位为字节 |
| `bufferUsage` | 同一样本的 32 位无符号 usage 位掩码 |

`pixelCopyCpuMs` 的分母是 `pixelCopyCpuSamples`，不是 Native 尝试次数。线程 CPU 时间与 `pixelCopyMs` 墙钟时间属于不同测量，不能互相替换，也不应仅凭差值断定调度、缓存或带宽原因。零个有效样本不会生成猜测的零耗时。`strideBytes`、`bufferUsage` 与既有尺寸字段描述最后一个样本；只有在尺寸和场景固定的对照中，才能据此讨论整段窗口的共同条件。

这组字段必须同时出现，且必须随当前 `mapCopyMs` 格式的完整四项细分出现。样本数和 stride 必须为非负整数，usage 必须在 `0` 至 `4294967295` 之间；正样本数对应非负有限 CPU 时间，零样本数对应 `none`。脚本拒绝缺项、错误类型和未知字段，不把不支持的行当作测量值。CPU细分扩展不改变原有耗时含义；RS统计列的schema 2更正与之独立。

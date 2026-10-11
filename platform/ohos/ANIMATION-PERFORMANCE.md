# MatePad 动画卡顿

本轮按设置打开／关闭、侧栏收起／展开、邮件及同线程消息切换分别采集。上一轮短时间呈现／拖动的平均 FPS 不能证明这些动画流畅。本页的完成状态以新包的同场景实机对照为准。

## 实机基线

2026-10-11，`inbox-layout-fixed-signed.hap`，CPU，2880×1786。每次只触发一个动作，清空的仅是本应用 Surface 计数，收尾等待约2.8秒；等待不会成为 FPS 分母。下表保留旧脚本 schema1 的第一列统计，现更正为提交时间间隔，不能称为实际呈现帧率。所有间隔，包括长帧和可能的输入命令空窗，均保留。Rust约两秒汇总，Native至少两秒且仅在提交时触发汇总，可能跨越长静止期；两者均可能跨动作边界，不能直接当成逐帧对应数据。

| 动作 | 有效槽数 | 提交间隔 p95 | 最大提交间隔 | >50ms / >100ms 间隔数 |
| --- | ---: | ---: | ---: | ---: |
| 打开设置 | 15 | 229.69ms | 229.69ms | 3 / 3 |
| 关闭设置 | 29 | 27.24ms | 196.88ms | 1 / 1 |
| 收起侧栏，自身回复线程 | 21 | 77.14ms | 85.90ms | 5 / 0 |
| 展开侧栏，自身回复线程 | 18 | 117.81ms | 117.81ms | 5 / 4 |
| 切换到自身短HTML消息，第1次 | 3 | 194.82ms | 194.82ms | 2 / 2 |
| 切换到自身短HTML消息，第2次 | 3 | 224.77ms | 224.77ms | 2 / 2 |
| 收起侧栏，自身短HTML | 15 | 138.51ms | 138.51ms | 4 / 3 |
| 展开侧栏，自身短HTML | 17 | 184.55ms | 184.55ms | 4 / 3 |
| 切换到同线程原始12行HTML消息 | 15 | 479.43ms | 479.43ms | 1 / 1 |

短HTML为自身合成线程中的第二封3行消息；原始消息才有12行及3个附件。两次手势尝试没有证实惯性滚动，其中一次触发正文选区，均保留为无法判断；不作为惯性帧率结论。匿名汇总是 `target/ohos-evidence/animation-current-baseline-summary.json`，每场景原始数字均独立保留。

## 时间戳口径与输入边界

公开 [RS Surface FPS 源码](https://github.com/openharmony/graphic_graphic_2d/blob/master/rosen/modules/render_service_base/src/gfx/fps_info/rs_surface_fps.cpp) 导出的是 `flushTime:presentTime`。旧脚本把第一列误标为 `actual_present_fps`；历史JSON保留原样，后续schema2用第二列计算报告的呈现间隔，第一列单独作为提交统计。设备二进制尚未与该公开master版本逐字匹配，因此同时保留两列原始数据与这一版本限制。

第二列独立重算仍确认严重长帧：原基线设置打开最大224.326ms、关闭199.319ms、展开侧栏116.609ms、短HTML切换199.307ms。不能用口径修正掩盖已复现的问题。

[RS manager源码](https://github.com/openharmony/graphic_graphic_2d/blob/master/rosen/modules/render_service_base/src/gfx/fps_info/rs_surface_fps_manager.cpp) 的清空操作只清除记录环，待处理的flush／present对应项可能随后写回旧时间戳；这与实机清空后出现旧槽一致，但不据此猜测设备内部容量。新诊断包在真实Touch／Mouse按下和抬起时记录 `CLOCK_MONOTONIC` 纳秒，日志只含动作0／1和时间。只有明确核对RS与输入时间处于同一时钟后，才按输入边界划分提交于本动作后的候选帧；全序列、边界前槽和原有截断标记继续保留。候选帧仍可能包含上个动作未完成的工作，不能自动等同于纯动画帧。

打开设置的Slint绘制最高117.41ms，展开侧栏的动画窗绘制最高96.07ms，短HTML切换的事件处理约96–102ms、Timer最高约61–62ms；这些场景Native提交最高约6ms。定位重点是应用绘制及同步正文重排，不据此盲改Native提交或VSync。

## 修复实现

- OHOS CPU模式设置 `MotionSettings.software-rendering`，整工作区、列表及正文画布保持不透明，继续保留工作区／列表的位移动画和小控件反馈。避免每帧对整页图文执行透明混色；其他平台默认行为不变。
- 正文 viewport 更新在尺寸每次变化时重启50ms Timer，等待安静区间后再同步重排。侧栏移动期间复用已有 tile，结束后按最终尺寸准确重排；非OHOS模式保留原50ms节流。
- 已合成不透明背景的OHOS CPU tile存为RGB8，缓存字节数减少25%；意外透明输入保留RGBA。锁定Slint1.18.1的36组真实绘制验证了透明度、缩放、位置、颜色和stride像素等价。主机5.14MP隔离测量没有证明RGB8稳定提速，实机增益不单独归因于格式转换。 当前Rust日志的`retained_tile_bytes`仍按4字节／像素估算，不能用它证明RGB源缓存的实际占用或Slint内部图像缓存总内存。
- OHOS debug应用代码优化级别升为2，保留断言／调试符号；绘制库仍为3。
- owner loop在处理事件前及绘制前更新Slint Timer／动画，统计全部调用且从事件耗时中扣重。Slint1.18.1本身已在指针处理中更新动画tick，旧tick导致Flickable惯性异常的初始推断未成立；本修复验证的是到期Timer先于输入回调，不能宣称它单独消除了惯性卡顿。

## 第一轮实机结果与继续定位

第一轮已完成ARM64 Rust构建19分45秒、签名HAP构建24.171秒及安装。包SHA-256为 `a34e2dea761a68e83972aaa7b48ce560f8b65f8229a82c5cf9e2ab20ed5722c5`，164881373字节；诊断冷启动PID6872。冻结的18项源码／配置一致，签名配置恢复一致。三轮设置、侧栏、短HTML及回复切换共18份数字报告均经独立重算，但**动画卡顿仍未解决**。

三轮设置打开的Rust绘制最高105.766／116.167／175.470ms、事件处理94.013–105.455ms；展开侧栏绘制最高87.714–109.241ms；短HTML切换事件85.479–99.315ms、Timer50.845–59.451ms。这些是匿名汇总窗口，不能逐帧对应单个动作。RS清空后仍可能延迟收到上个动作的旧时间戳，即使输入前即时快照为零，随后也可出现约3–5秒旧动作空窗。因此全序列及所有间隔保留，动作FPS归因标为无法判断，不把这类空窗当成绘制回归，也不手动删慢帧来证明改善。独立记录见 `animation-optimized-independent-audit.json`。

独立静态检查确认设置页和侧栏最终几何正常；自身原始HTML的1360×820裁剪与旧包解码后1115200像素完全相同。设备没有现成画面录屏命令；静态画面不构成流畅度证据。

对同一PID做12秒、仅自身用户态的CPU周期采样：2504条、0丢失，重复自身UI的设置与侧栏操作。匹配ELF的`.text`及BuildID与签名HAP完全一致，BuildID为 `1bf99ab70f87c06bb24dc21087ff79aa8489466c`。最高热点集中在tiny-skia0.12的像素流水线、bytemuck1.25.2的NEON转换及相关UB检查。此前OHOS debug profile没有覆盖这条独立的SVG绘制依赖链，仍为默认opt-level=0；不能因Slint主crate已优化就假定整个绘制调用链已优化。

独立审计按flat self rows的叶级PC互斥归类，不累加inline链：tiny-skia29.017178%、bytemuck9.372613%、core UB／copy前置检查8.687649%、tiny-skia-path2.700367%，合计49.777807%整次用户态周期。202条唯一函数行覆盖76.972235%周期，余下23.027765%低于报告阈值，保持未归类；不能把这些比例当成单个动画的耗时或FPS，也不能将全部core检查自动归因于SVG。

第二轮补齐bytemuck、tiny-skia／tiny-skia-path的两个固定版本，以及resvg／usvg／kurbo／svgtypes／strict-num的opt-level=3，继续保留debug断言；另增加默认关闭的原生数字输入边界。没有把尚未证实收益的设置懒构造或正文拆批混入该包，以便关联实际热点与修复效果。ARM64 Rust实际构建21分34秒，22个本轮目标crate实例的编译参数已白名单核对；实际启用的tiny-skia0.12.0／resvg0.48.1／bytemuck1.25.2等均为opt3、断言开启。清单里配置的0.11.4实例不等于本次一定编译。签名包与实机效果以第二轮采集为准。

隔离主机SVG对照使用同版本resvg0.48.1／tiny-skia0.12.0以及实际search、sync、inbox-flectar图标，比较24／45／54／144px共12组，opt0及opt3两边保留debug断言，fixture本体固定opt2。四次交错执行的premultiplied RGBA全部字节一致；每组每profile14轮×32次，包含pixmap分配与resvg绘制的中位耗时降低12.72–16.82倍。54px的search为557.48→39.56µs、sync675.50→50.48µs、inbox1459.37→102.30µs。仅证明主机像素等价及热点优化方向，不能外推平板FPS。证据为 `svg-profile-host/validation.json`。

## 第二轮实机结果

签名HAP构建29.096秒、安装及诊断冷启动通过，包为 `animation-vector-optimized-signed.hap`，163272745字节，SHA-256 `46db7299a9057960d7d6ef7f2738905d8fd6715edf0eaeb21ac6e721dbebf637`，PID24229。19项最终bundle源码与签名配置恢复匹配；对应ELF的BuildID为 `b7c9c8efd4ddef99a62631f753ab3335e042340d`，与签名包`.text`匹配。

输入时钟独立probe支持本次session两列与Native输入的同时间起点；没有加时钟偏移，公开RS设备版本仍未匹配。三轮共18个独立动作记录均保留完整序列、按下及抬起下界候选、旧槽和跨动作空窗。最早的输入后候选帧可能仍在显示按下反馈，不能用其时间证明目标页面已出现；候选跨度也不是纯动画起止。

18份数字经独立实现重算一致，见 `animation-vector-optimized-independent-audit.json`。不带性能参数正常冷启动PID30540，触控后新增性能／输入边界日志均为零，确认诊断默认关闭。五个关键态静态画面正常，自身原始HTML同1360×820区域与旧包解码后1115200像素完全一致；该检查不构成动画流畅证据。

| 动作 | 三轮候选最大呈现间隔 | 对应汇总窗口中的最高开销 | 结论 |
| --- | --- | --- | --- |
| 收起侧栏 | 33.234／24.979／24.964ms | 绘制最高23.701ms | 三轮均无>50ms候选间隔，明显改善 |
| 展开侧栏 | 33.325／41.437／33.249ms | 绘制最高31.670ms | 三轮均无>50ms候选间隔，明显改善 |
| 打开设置 | 141.221／124.901／141.288ms | 事件100.578ms，绘制44.267ms | 同步事件仍阻塞，未通过 |
| 切换短HTML | 174.394／157.790／199.343ms | 事件88.410ms，Timer56.769ms，绘制38.140ms | 正文工作仍阻塞，未通过 |

关闭设置的按下候选最大132–141ms包含按住过程；抬起后的三轮最大间隔均不足25ms，不能把前者直接宣称为关闭动画卡顿。回复切换仍有66–75ms候选间隔。设置及HTML在抬起下界之后仍有上述长间隔，不能因平均值较高而判流畅；本包只解决了一部分问题。第二PID同12秒自身用户态采样另有2473条、0丢失，用于定位残余热点，未把采样周期换算为动作墙钟耗时。

第二次采样为7244060147个用户态周期，flat self报告这次保留全部阈值，932条唯一函数行覆盖99.999472%。互斥叶级归类：read-fonts0.41.0占14.767698%、font-types0.12.6占8.328328%、harfrust0.12.0占1.574685%，直接叶合计24.670711%；tiny-skia／tiny-skia-path／bytemuck合计9.086233%，core UB检查另0.292068%。旧报告阈值为0.1%，未报告长尾较多，不能盲比归类覆盖，也不能将两次周期比例换算为单动作延迟。

正常模式PID30540、诊断关闭的邮件专用12秒采样将设置／侧栏排除：九次自身短HTML与回复切换均在录制内，2723条样本、8627255747个用户态周期、0丢失。1235条flat self函数行覆盖99.9990205%，独立逐叶核算前三个字体依赖合计29.622333%，全部在UI线程。Stylo／生成代码／cssparser／selectors联合6.081107%；不能据此把CSS认定为首要瓶颈或盲目扩大优化范围。逐像素迭代的11.856872%属于core叶级组，inline链用于关联已优化Slint渲染路径，不能再次累加，也不是core未优化的证据。该工作负载与混合采样不同，比例不是前后增益。证据为 `animation-vector-html-hiperf-independent-audit.json`。

真实rustc调用确认前三个字体依赖此前仍是opt0，上层Parley／Skrifa的opt3不能覆盖依赖中的表读取。第三轮只为采样命中的read-fonts0.41.0／font-types0.12.6／harfrust0.12.0补opt3，继续保留断言和锁定版本。另一写信编辑器的read-fonts0.37／font-types0.11.3／harfrust0.5.2，以及带字形缓存的Glifo，缺少本次直接叶证据，保持现有配置。

进一步对第二包17个逐像素热点PC检查ELF DWARF Compile Unit，均来自feihong-rust，两个编译单元分别覆盖8.298903%与3.557969%，合计上述11.856872%。因此不能仅凭Slint源码路径推定这些泛型实例按renderer的opt3编译；真实feihong-rust命令仍为opt2、断言开启。该行为符合 [Cargo泛型与profile说明](https://doc.rust-lang.org/cargo/reference/profiles.html#overrides-and-generics)。最高Range PC是循环比较／回边，不能将整个权重解释为独立next函数调用开销。报告为 `animation-vector-opaque-generic-cu-independent-audit.json`。第三包范围保持冻结；若绘制仍慢，可另做只改变wrapper为opt3的单变量设备对照，收益必须测量，不能从等级数字推断。

同版本Slint1.18.1的隔离主机caller／泛型对照，采用真实OpaqueRgba、1920×1200缓冲区、1.94MP RGB纹理移动、半透明覆盖层与小文字控件，两个profile均保留断言和溢出检查，按2／3／3／2四次执行。192个画面的SHA256及damage尺寸完全相同。四轮中位数：RGB纹理4.337／4.334／4.265／4.326ms，半透明10.892／10.943／10.770／10.795ms，文字1.587／1.683／1.626／1.567ms，**未证明opt3稳定提速**。预编译的非泛型renderer／字体代码完全共享，但编译参数并非由runner重新验证；实际OHOS输入、提交、字体和VSync均不在此fixture内。主应用同时编译且未控温／频率，不将单个outlier下降作为收益。证据为 `target/ohos-host-contracts/opaque-profile/validation.json`；当前生产wrapper保持opt2。

## 第三轮范围及设置状态契约

本轮在字体三项opt3之外，只延迟OHOS CPU设置页中尚未访问的六个非General页面和四个弹窗的首次构建。第一次显示后保留实例直到整个SettingsDialog销毁；滚动位置、输入字段与弹窗状态由原组件保留。General、ThemeEditor及设置关闭的预览回滚逻辑保持原实现；其他渲染模式继续一次构建全部页面。

锁定Slint1.18.1的host契约编译实际SettingsDialog、ThemeEditor与完整依赖UI，仅临时添加构造／实际viewport观测和原函数入口。六项运行测试验证首次General／Accounts构建、真实wheel与复访保留、搜索选页、非CPU eager、四弹窗开关、跨页主题预览和两次取消、整个dialog销毁重建。移除visited latch的负对照会因实际ScrollView实例及位置丢失而失败。生产源码SHA为 `5053049e90461fc93771a0eb74dd77bcba8fe4ae5a45b68b3e4a54ffd6a4ea78`，报告为 `target/ohos-host-contracts/settings-lazy/validation.json`；主机预嵌资源表示与设备不同，不能作为设备外观、IME或性能验收。复现见 [契约说明](tests/SETTINGS-LAZY-CONTRACTS.md)。

第三轮ARM64 Rust实际构建23分34秒，签名HAP构建24.589秒并安装到同一MatePad。包为 `animation-font-lazy-optimized-signed.hap`，161823287字节，SHA-256 `5c58343dbc8016a9f09f71413d387c260723d54c9981ec7c1718cea11e3ff254`，诊断PID55239。20项冻结源码／配置与签名配置恢复匹配。本轮实际目标rustc调用确认上述三个字体crate均为opt3、断言开启；缓存依赖的既有参数另列，不冒充本轮重新编译。匹配ELF的BuildID为 `f0d6e3f69da0d608b462703699a005b3480552ac`，`.text` SHA-256为 `3b3de1440d3f91393caded7ff2381c8b946cc6cc451be3870484cca61025c18d`。

三轮同样六场景共18份schema2记录，经独立实现逐数字重算一致。下表同时披露抬起下界与完整按下下界；最早候选画面可能只是输入反馈，不能用它证明页面完成。三轮全部18份抬起后候选间隔没有超过50ms或100ms，HTML每轮仍有一次超过33.33ms。独立报告为 `animation-font-lazy-optimized-independent-audit.json` 和 `animation-font-lazy-optimized-comparison-independent-audit.json`。

| 动作 | 第二包抬起后最大间隔，三轮 | 第三包抬起后最大间隔，三轮 | 第三包补充限制 |
| --- | --- | --- | --- |
| 打开设置 | 141.221／124.901／141.288ms | 24.976／24.953／33.287ms | 按下下界同样没有>50ms间隔；事件窗口最高16.245ms，绘制19.834ms |
| 切换短HTML | 174.394／157.790／199.343ms | 49.865／49.913／49.808ms | 完整按下候选仍各有83.059／83.101／83.032ms间隔；事件51.684–54.746ms、Timer34.244–36.868ms |
| 关闭设置 | 三轮均不足25ms | 16.640／16.651／16.739ms | 完整按下候选124.566／141.164／141.253ms包含约100ms按住过程，原始数字保留 |

侧栏收起／展开及回复切换的18场对应记录同样保留全部槽、旧槽和按下／抬起候选，没有删去静止空窗。设置懒构造与字体profile合在一个包中，不能将增益分别归因于其中一项。

首次General、首次Accounts、首次Contacts及返回General的设备画面和导航正确，未修改设置或账户。自身短HTML的979200像素与原始12行HTML的1115200像素裁剪，与第二包解码后像素全部相同；这是最终静态画面检查，不是动画验收。

冷启动首次邮件操作仍单独保留：按下到首候选呈现255.789ms，按住101.547ms，抬起到首候选呈现154.242ms，后续仍有91.360ms间隔；对应约两秒Rust窗口的事件最高216.176ms。原probe未确认时钟的无法归因状态保留，后续同包独立时钟核对支持上述另行重算；设备RS版本仍未逐字匹配。该停顿没有被18场暖操作结果覆盖，继续以重复冷启动及匹配ELF采样定位。

两次重新启动独立复现首候选等待160.737／162.840ms、事件窗口最高222.781／226.284ms。正常模式冷启动PID62847、首次自身HTML显示正确，新增性能／输入边界记录均为0；正常模式没有原生输入边界，未伪造同口径响应时间。第三包真实Scrollbar反向swipe的按下／抬起闭区间内72帧，p95为16.661ms、最大24.947ms、无>50ms；完整108帧仍保留跨抬起的398.620ms间隔。另一次长按drag保留1245.884ms间隔，不据命令时间窗推定一直在运动或惯性流畅。闭区间属于子集，仍沿用原384槽环的截断状态，不将人工子集大小误作真实环容量。前后480760像素正文裁剪变化、反向恢复与原图完全一致，确认真正滚动。

第三包12秒启动／首邮件混合用户态采样为705条、1434161861周期、0丢失，315条flat self函数行完整覆盖。匹配ELF的真实调用链包含`build_email_font_ctx`、线程局部首次初始化和`std::fs::read`进入原邮件字体构造函数。混合窗口的函数比例不等于首事件占比，CPU周期也不包含全部I/O等待；字体叶热点接近零不能排除字体初始化的墙钟延迟。另一个探测缺口是原`email prepare + initial layout`计时在字体上下文已作为实参构造后才开始，完全漏掉首次字体bootstrap。

第三包正常模式的另一份12秒邮件专用采样为2884条、6667662507周期、0丢失，九次自身短HTML／回复切换均在录制内。1210条flat self行覆盖99.999510%；三个字体库的互斥直接叶占比为0.462917%，CSS／style／selectors为8.579545%、HTML parser为0.289217%、drawing及core UB为8.439710%。逐像素绘制inline上下文仅用于定位，不能重复累加。两包均为正常模式邮件专用操作，但第三包另包含设置懒构造，比例不是字体改动的独立因果证明，也不是动画墙钟或FPS。证据为 `animation-font-lazy-warm-html-hiperf-independent-audit.json`。

## 第四轮冷启动字体预热

本轮只改renderer、应用入口及OHOS Rust入口三处：Tokio runtime建立、平台及emoji字体已配置后，在`AppWindow::new`前启动后台字体构造；完成后发布一份独立邮件`FontContext`模板。界面线程只对已完成的`OnceLock`做`get`／`clone`，不等待后台初始化。原字体构造函数字节及路径／fallback算法保留；没有共享可写Slint字体集合。后台失败或首用尚未完成时沿用原同步构造，保留功能，但该fallback仍可能占用界面线程。

仅显式debug `feihongPerformance=true`注册数字字体诊断callback：记录background、success、published、ready和bootstrap毫秒。默认关闭和release路径不会取诊断时钟、格式化或输出该日志，也不修改环境变量。原来的广义`FLECTAR_RENDER_TIMINGS`环境设置不会隐式开启此日志。第四轮检测会核对后台完成和主线程ready命中，避免只从后续帧间隔推断初始化收益。

六个实际源码抽取host运行案例覆盖已就绪模板、后台未完成时的原fallback、后台panic、模板clone隔离、显式诊断及默认／release关闭。多family TTC模板clone保持五脚本fallback、SansSerif的FamilyID和33个中文／繁体／Latin／数字／emoji字形选择；跨独立Collection重建时Fontique既有HashMap迭代顺序不固定，未将其改成另一种字体排序。OHOS debug／release两项实际helper metadata编译通过，包括`spawn_blocking`与`OnceLock<FontContext>`；这是编译检查，不是设备运行验收。报告为 `target/ohos-host-contracts/font-prewarm/validation.json`。

三项应失败负对照分别覆盖模板共享导致clone可写状态泄漏、后台panic后错误发布默认模板，以及界面线程等待未完成任务。用户要求本轮尽快收尾，第四包以三次首次邮件操作、单轮六场暖态回归、实际滚动及正常诊断关闭为设备验收范围，不再开展下一轮优化。

第四轮完整ARM64 Rust构建34分54秒，签名HAP构建31.935秒，安装成功。包为 `animation-font-prewarmed-signed.hap`，161835464字节，SHA-256 `2e24b143746e92cf8ce158985e78e18ded7939fdea0144400dceec3570a7e3aa`。21项冻结源码核对通过，实际应用及桥接rustc均为opt2、断言开启；字体三库沿用第三包opt3缓存，不冒充重新编译。保留ELF的BuildID为 `10eb1228644134c39ce8bb8d085d69ea6b8ed27b`，`.text` SHA-256 `0490b1215e2b5f366f4f0fe2d95103853100447f1eb711df93b65d78d46f1fc8` 与签名HAP匹配。签名配置原字节和权限恢复，账户数据保留；未安装或使用Simulator。设备回测结果另列。

三次进程冷启动均成功发布字体模板，首用均为`ready=1`。第一轮沿用第三包的3秒启动步骤，另两轮仅固定等待0.5秒，再读取自身PID／Surface后立即点击；没有等待后台字体日志才操作。后台构造201.49–290.12ms，首次界面模板初始化0.14–0.31ms，后者不包含最终TLS clone和日志输出。并行后台耗时不等于界面阻塞。

| 首次自身HTML，进程冷启动 | 第1轮 | 第2轮 | 第3轮 |
| --- | ---: | ---: | ---: |
| 抬起到首候选呈现 | 55.831ms | 93.639ms | 90.775ms |
| 抬起后候选最大呈现间隔 | 58.178ms | 58.135ms | 49.766ms |
| 事件汇总窗口最高 | 53.576ms | 54.551ms | 53.192ms |
| Timer汇总窗口最高 | 38.131ms | 37.459ms | 34.781ms |

相比第三包154–163ms的首候选等待、216–226ms事件窗口，首次字体阻塞明显减少，但第一、二轮仍各有一个>50ms候选间隔，未称完全无卡顿。完整按下候选最大132.936／99.662／91.390ms及第三轮一个旧槽仍保留；候选首帧可能是输入反馈，不能证明该时刻正文已完整显示。双列、nearest-rank及真实按下／抬起边界已独立复算一致，原始记录为 `animation-font-prewarmed-cold-{startup,first-html}-r{1,2,3}.json`。

第四包暖态只做同样六场的单轮回归，独立重算通过；不能与第三包三轮样本直接作稳定性等价判断。

| 暖态动作 | 抬起后候选p95 | 最大间隔 | >50ms / >100ms |
| --- | ---: | ---: | ---: |
| 切换回复 | 8.417ms | 16.630ms | 0 / 0 |
| 打开设置 | 41.589ms | 41.589ms | 0 / 0 |
| 关闭设置 | 16.653ms | 16.674ms | 0 / 0 |
| 收起侧栏 | 16.723ms | 24.960ms | 0 / 0 |
| 展开侧栏 | 16.645ms | 16.665ms | 0 / 0 |
| 切换短HTML | 16.800ms | 58.112ms | 1 / 0 |

短HTML首候选呈现仍等66.383ms，事件窗口52.499ms、Timer42.086ms、Slint绘制13.44ms；关闭设置的完整按下候选仍有124.580ms间隔，所有原槽保留。性能问题明显减轻，仍有短停顿，依照用户收尾要求不追加下一轮。自身短HTML的979200像素及General页3378400像素与第三包完全一致，根代理目视检查中文／emoji及布局正常；静态画面不能证明动画流畅。

一次真实滚动条操作前后正文480760像素变化，标题及前3行滚出、附件进入视窗，确认实际偏移。原生按下／抬起闭区间内63帧／62间隔的p95为16.660ms、最大16.737ms、无>50ms；完整按下候选98帧仍保留373.727ms跨抬起空窗，原100帧全环另有两旧槽及2541.394ms间隔。1068.093ms的Native持有时间不等于持续运动或惯性，未称持续FPS通过。子集继承384槽、284空槽及未截断标志，最初遗漏标志的JSON已另存0600历史副本并修正metadata，原始双列和帧统计不变。证据为 `animation-font-prewarmed-static-scroll-validation.json` 及 `animation-font-prewarmed-scrollbar-swipe.json`。

最终普通模式新PID29255，字体初始化、Rust UI、Native呈现及输入边界诊断新增记录均为0，正文裁剪与诊断模式解码后像素完全相同，账户保留。同一PID的`onNewWant`已成功恢复`feihongPerformance=false`和`feihongKeepScreenOn=false`，关闭调试强制亮屏。记录为 `animation-font-prewarmed-normal-mode-validation.json`。本轮到此收尾，不追加采样或功能；偶发58ms邮件间隔及56–94ms冷首候选等待仍是实际限制。

最终来源／设备记录为 `animation-font-prewarmed-validation.json`，21项冻结源码全部一致；当前安装状态已更新，第三包历史独立保留。独立审计 `animation-font-prewarmed-final-independent-audit.json` 的23项检查／10份采集通过，覆盖三冷、六暖和滚动双列／真实边界／全部间隔，以及普通诊断关闭；静态像素列为设备操作者证据，根代理另行目视核对。结论是已明显改善、仍有短停顿，验收仅覆盖这一台平板及上述场景。

## 重复检测与回归

使用 [性能采集脚本](../../scripts/ohos-perf.py) 中的同一限定PID／Surface采集流程。[schema2及输入边界说明](MEASUREMENT-SCHEMA.md) 明确第二列、时钟确认与历史证据保护。新的 `fps.frame_pacing` 保留超过33.33、50、100ms的间隔数量、比例和最长连续次数；不会删除慢帧，不从时间戳猜测动画或惯性，也不把静止期判成卡顿。短动画须记录具体动作并查看p95、max与长帧数量；平均FPS超过30仍可能存在明显顿挫。

脚本现有55项自测通过；新增严格数字字体初始化解析后，72份已有匿名dump双列及6个动作边界案例与旧版完全一致。旧首列评估负对照按预期失败。生产owner loop25项契约与3组旧实现负对照通过；viewport scheduler3项契约及旧节流负对照通过；RGB tile3项契约通过。原生输入边界以API26 SDK完成debug／release条件各两项syntax-only检查；运行时仍须实机验证。独立source review保存于 `target/ohos-evidence/animation-pacing-source-audit.json`，新增字体检测回归为 `ohos-perf-font-init-detector-regression.json`。

```sh
python3 scripts/ohos-perf.py --self-test
python3 platform/ohos/tests/run_host_contracts.py --deps-dir /path/to/host/debug/deps
python3 platform/ohos/tests/run_viewport_animation_contracts.py --deps-dir /path/to/host/debug/deps
python3 platform/ohos/tests/run_opaque_tile_contracts.py --deps-dir /path/to/host/debug/deps
python3 platform/ohos/tests/run_svg_profile_contracts.py --output-dir target/ohos-host-contracts/svg-profile
```

主机契约不替代实机流畅度。第四包已减少首次邮件的阻塞，剩余短长帧仍如实保留；CPU默认及原账户保持。

# 帧时间测量 schema 2

`scripts/ohos-perf.py` 的 `analyze_fps(text)` 默认使用 RS 数字对的第二列计算 `actual_present_fps`、p95、max 和长间隔数量。公开 [Rosen 源码](https://github.com/openharmony/graphic_graphic_2d/blob/master/rosen/modules/render_service_base/src/gfx/fps_info/rs_surface_fps.cpp) 输出 `flushTime:presentTime`；`timestamp_semantics` 保留来源和 `device_source_version_verified=false`，因为该公开 master 尚未与设备 RenderService 二进制匹配。数值可复算，列语义仍有版本限制。

结果包含 `schema_version=2`、`timestamp_column=second_present`、原始数字 `ring_pairs_ns`、排序后的第二列及全部间隔；`flush_stats` 独立保留第一列的完整统计，速率名为 `observed_flush_fps`。两列分别检查重复时间戳；重复 present 时刻不会重复计帧产生 FPS。所有合法间隔均保留，包括输入命令间空窗和静止期，不能直接把这些统计解释为纯动画轨迹。

schema 1 历史 JSON 保持原样，其原有数字来自第一列，不能继续称已确认的呈现 FPS。CLI 的 `--output` 必须指定新文件，避免覆盖既有证据。连续手势评估要求 schema 2 第二列、明确的连续操作声明、足够覆盖以及未截断的 ring；不会接受 schema 1 或第一列报告来判定通过。

## 原生输入下界

匿名白名单新增下列日志，必须来自指定应用 PID 的 `FeihongPerf` tag：

```text
Native input boundary action=0 monotonicNs=33136000000000
```

`action` 仅接受整数 `0` 或 `1`，`monotonicNs` 仅接受大于零的 uint64。解析为 `{"kind":"native_input_boundary","values":{"action":0,"monotonic_ns":33136000000000}}`。坐标、按键、文本、附加字段、其他 PID 或其他 tag 均不进入匿名结果。

调用 `analyze_action_fps(text, input_ns, clock_monotonic_confirmed=True)` 前，采集方须取本次新日志中的真实 `monotonic_ns`，并显式确认 RS 时间戳与原生 `CLOCK_MONOTONIC` 共享时间基准。未确认时结果为 `inconclusive`，保留全统计，不划分任何槽位。不能使用 Mac 时间、命令发送时间、零快照或大 gap 推算输入时间。设备版本未匹配的限制不会因该布尔参数消失。

确认后，函数把 `flushTime >= input_ns` 的记录列为候选，保留 `candidate_fps` 的双列统计和 `unattributed_pairs_ns` 中的旧记录，并通过 `all_fps` 保存完整 ring 与双列全统计。迟到旧记录即使 presentTime 已在输入之后，也以其旧 flushTime 留在未归属记录中。候选集合继承原始 ring 的空槽数量、总槽数量和截断标志，不能因为过滤后全是非零记录而误报截断。

这是输入时间的下界，不是动画起止证明：不会猜测动画结束，不会自动移除任何候选长间隔，也不用于连续手势通过判定。一次录制包含后续动作时，这些动作仍可能出现在候选中，应保持归因限制。

离线命令保留原有参数，并可增加输入下界：

```sh
python3 scripts/ohos-perf.py --fps-file capture.fps.txt --output capture-schema2.json
python3 scripts/ohos-perf.py --fps-file capture.fps.txt --output capture-bounded-schema2.json \
  --input-monotonic-ns 33136000000000 --confirm-clock-monotonic
python3 scripts/ohos-perf.py --self-test
```

第二条命令的数字仅为格式示例，实际采集必须使用本次原生日志。原始输入文件和历史报告不被修改；输出权限为 0600。确定性测试覆盖首列匀速但第二列存在长帧、重复 present、迟到旧槽、未确认的时钟、截断标志继承、匿名白名单和 CLI 历史证据保护。

## 首次邮件字体初始化

显式 debug 性能模式另由指定应用 PID 的 `Feihong` tag 输出以下数字格式，匿名解析为 `kind=font_init`；默认模式与 release 不启用：

```text
FeihongFontInit background=1 success=1 published=1 bootstrap_ms=123.45
FeihongFontInit background=0 ready=1 bootstrap_ms=0.12
```

第一条记录后台构造及发布结果，第二条记录界面线程首次使用是否命中已完成的模板。标志仅接受0／1，耗时必须有限且非负；附加字段、路径、文本和其他PID均拒绝。`ready=0` 表示仍走原同步构造，不能仅凭后台任务已启动就宣称避免了卡顿。`published` 不得大于 `success`。

逐动作采集会减去动作前已有日志，因此后台启动记录须另在同一新PID的启动窗口采集。不能把被快照减掉的记录误报成未执行预热，也不能把背景初始化耗时当作界面阻塞时间或帧率。新增解析没有改变原双列与输入边界统计：55项自测通过，72份已有双列报告及6个动作边界案例与旧版结果一致。

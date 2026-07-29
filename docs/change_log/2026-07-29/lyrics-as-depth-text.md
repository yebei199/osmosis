# 歌词链:行级时间轴与深度文本

## 1. Change Purpose

播放页第二步的第一轮(#11)交付了粒子与封面深度卡,但深度卡片当时只有封面这一个
「装饰」载荷 —— 共识里真正要验证的载荷是歌词。本轮把它补上:歌词从上游一路接到
播放页,当前行随播放进度走,粒子从字的前后穿过。同一轮里顺带补齐了粒子密度对齐
参考图(用户反馈「粒子太少了」,并给了 Mineradio 源码)。

## 2. Change Scope

五个 crate 加参考资产,全为功能新增:

- `crates/contract`:加 `LyricLineDto` / `LyricDto`。
- `server`:`bangdream.rs` 加 `lyric_to_dto` 与四条转换测试;`main.rs` 加
  `/lyric/{track_id}` 路由。
- `crates/api`:加 `lyric()` 与 `lyric_url()`。
- `crates/app-core`:新模块 `lyric.rs`(`current_line` 纯函数 + 六条测试)。
- `crates/ui`:`app.slint` 加歌词块与第二个遮挡裁剪矩形;`music.rs` 加 `LyricFeed`
  并在换歌时取歌词;`lib.rs` 每帧选行推送;README 同步。
- `crates/render3d`:`particles.rs` 行为模型重写(见第 3 节第 0 步)。
- `docs/reference/play-page/`:三张阶段性参考图(做完即删)。

## 3. Implementation Process

0. **粒子密度对齐**(8985140、de7b63e):先按「连续散列轨道、480 颗」加密,用户
   给出 Mineradio 源码后发现方向不对 —— 原版不是轨道系统,是**浮空尘埃层**
   (`01-float-skull-backcover.js` 的 `createFloatLayer`)。第二次按源码逐常数
   转写:1300 颗、76% 压扁椭圆晕圈 + 24% 散射盒、极慢整体旋转 + 三轴正弦漂移 +
   呼吸,低频只轻推纵深,闪烁折进缩放(原版是 alpha 振荡,逐帧改上千份材质不划算)。
   三条新测试先 RED 打在旧轨道模型上,再实现到 14/14 GREEN。
1. **契约与桥**(177e55a):行级时间轴。刻意保留一处与 play 路由不同的语义 ——
   歌词缺席**不是失败**,给空行表;纯音乐与未收录都会走到这里,报成错误会在界面上
   显示成故障。
2. **api 与选行**(831a005):`current_line` 的判据是**下一行的开始时刻**而不是
   本行的 `end_ms` —— 平台常常不给或给错 `end_ms`,而间奏里保持上一行是通行做法。
   线性扫描不用二分:行表撑死几百行,而二分要求时刻单调,脏数据不保证。
3. **深度文本**(9807423):歌词与封面同处一个 z 平面,于是**同一张遮挡图**裁两次
   (封面矩形、歌词矩形),粒子因此也从字的前后穿过。`LyricFeed` 带代际计数器,
   UI 靠 `(代际, 行号)` 判断该不该推新值 —— 每帧无脑推会标脏,暂停定格与失焦
   零重绘就都白设了。

## 4. Key Diff / Core Functions

- `bangdream::lyric_to_dto`:上游行表 → 契约行表,空串译文转 `None`。逐字档位下
  上游已把整行 `text` 拼好,行级消费方因此不必关心上游给的是哪一档。
- `main::lyric`:`response.lyric.unwrap_or_default()` —— 缺席即空行表,不报错。
- `app_core::current_line(lines, position_ms) -> Option<usize>`:最后一个已开始的
  行。前奏给 `None`,间奏与尾奏保持上一行,空表与乱序都不 panic。
- `ui::music::LyricFeed`:行表 + 代际 + 播放器句柄;`current()` 读位置选行,
  `replace()` 换歌时整批替换并递增代际。
- `ui::run_with_renderers` 的歌词段:只在覆层展开时跟随,只在 `(代际, 行号)`
  变化时写属性。

## 5. Verification

- 单元测试:app-core 31 条(含新 6 条,RED 先行)、server 30 条(含新 4 条)、
  api 8 条(含新 1 条)、render3d 14 条(含粒子新模型,RED 先行)、ui 30 条,全绿。
- 接口实测:`GET /lyric/2653192670` 返回带时间轴与中文译文的行表。
- 真实像素验收(桌面 linux,niri):歌词与译文在封面下方居中上屏;**字上压着粒子**
  (按 .slint 层序,只可能来自裁到歌词矩形的遮挡层);相隔 9 秒两张截图,行从
  「I'm doing fine, alright, okay, sun」走到「But I'm taking my pills, and I'm
  paying my bills」,时间轴跟随成立。
- 已知未覆盖:android 未构建验证(欠账同上一轮);逐字卡拉 OK 未做;歌词区目前只
  显示当前行,不做上下文行滚动;取不到歌词的降级路径未在真机上专门构造。

## 6. Difficulties

- 粒子第一次改错了方向(把参考图读成「更密的轨道」),用户给出源码后才发现原版是
  浮空尘埃层。教训:有源码就先读源码,不要从截图反推行为模型。
- 验收时上游音乐接口整体超时(不是本仓问题),中断过一次;用户重启 Go 后端后恢复。
- axum 服务端是长驻 dev 进程,加了路由后必须重启才生效,一开始 `/lyric` 空响应
  就是这个原因,不是代码错。
- 退出路径的 TLS 析构 abort(issue #15)在本轮多次复现,按用户决定后置。

## 7. Final Result

歌词是深度卡片的第一个真载荷,现在真的在跑:行随进度切换,粒子从字的前后穿过,
与封面卡共用同一张遮挡图。粒子密度与参考图对齐。契约新增两个类型,协议版本未动
(新增路由与类型是兼容变更)。

## 8. Risks And Follow-ups

- android 构建与真机发热读数持续欠着(粒子已升到 1300 颗,读数更有必要)。
- 逐字时间轴(卡拉 OK 高亮)未排期;契约留了扩展位。
- 歌词只显示当前行;要不要做上下文行与滚动,待定。
- 退出崩溃 issue #15 待修。
- `docs/reference/play-page/` 是阶段性资产,播放页收尾时删除。

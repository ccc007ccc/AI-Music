# `midi-agent-skill` 对本项目的参考价值

调研日期：2026-08-19

审阅对象：[`tubone24/midi-agent-skill`](https://github.com/tubone24/midi-agent-skill/tree/19625c682a18e2c7471890c4cdf8568e291b1765)

固定版本：`19625c682a18e2c7471890c4cdf8568e291b1765`（本次调研时 `main` 的提交）。

## 结论

这个项目值得借鉴的是 **AI 工作流的形状**，不是它的 MIDI 内部格式或通用 GM 路线。它把模型引导到几个确定性 Python 脚本，并把音乐知识拆成按需读取的参考文件；这能减少模型临时写生成器和一次性加载全部知识的问题。[`SKILL.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/SKILL.md#L6-L16) [`README.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/README.md#L17-L41)

本项目已经在更深的层次解决了它没有解决的事情：`Project` 是带 PPQ、tempo map、拍号、clip、力度和控制事件的权威状态；`ProjectEngine` 提供事务、revision 和撤销/重做；`composition-engine` 提供任务授权、范围、受保护区域、提案审查和一次性会话。参考项目适合作为 **AI-facing 适配层和知识组织案例**，不能替代这些 Rust seam。[`music-core`](../crates/music-core/src/lib.rs) [`composition-engine`](../crates/composition-engine/src/model.rs) [`reviewer`](../crates/composition-engine/src/review.rs)

## 可以吸收

### 稳定工具负责产物，Skill 负责决策

上游明确要求模型使用 `normalize_composition.py`、`generate_midi.py` 和 `convert_to_wav.py`，不要为每次请求临时写 Python/JavaScript。[`SKILL.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/SKILL.md#L8-L14) 这条原则应保留，但把“稳定工具”换成 `musicctl` 与 Rust crate：模型提交结构化 proposal，Rust 负责校验、应用、渲染和导出，模型不能以临时脚本绕过授权。

推荐链路：

```text
context/events
  -> 钢琴专用 brief 与 plan
  -> proposal/patch
  -> Rust review -> apply
  -> render/play -> critique -> bounded revision
```

它与本项目现有 [收敛循环](../skills/ai-music-composer/references/workflow.md) 一致，而不是照搬上游的一次性 `generate_midi` 调用。

### Progressive disclosure

上游根据任务选择 `music-theory.md`、`rhythm-patterns.md`、`voice-leading.md` 等文件，而不是每次读取整个知识库。[`SKILL.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/SKILL.md#L25-L43) 这个组织方式适合本项目，但内容应收缩成钢琴专用的三组：节奏/拍号、钢琴声部与踏板、动机/曲式。现有 [`piano-writing.md`](../skills/ai-music-composer/references/piano-writing.md)、[`rhythm.md`](../skills/ai-music-composer/references/rhythm.md) 和 [`form-and-development.md`](../skills/ai-music-composer/references/form-and-development.md) 已是更合适的起点。

每个知识文件还应带来源、适用范围和“建议还是硬约束”的标签，避免把某一种古典写法误当成钢琴音乐的语法边界。

### 面向模型的可读表示

上游用 `C4`、`F#5` 和 `"4"`、`"d4"` 这类音名/时值表示，确实比裸 MIDI 整数更容易让模型检查旋律轮廓和节奏。[`music.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/midi_types/music.py#L9-L28) [`SKILL.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/SKILL.md#L94-L127)

本项目可增加一个 **无损 AI 视图/解析器**：同时显示小节、拍位、音名、绝对/clip-local tick、时值、力度、踏板和声部角色；转换结果仍以 Rust `Project` 的 tick IR 为准。字符串时值不能成为第二个权威格式，也不能丢失非网格位置、重叠音和控制事件。

### 显式资源与产物边界

上游把脚本、参考资料、soundfont 和 `output/` 分开，并说明 MIDI→WAV 需要音源资源。[`README.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/README.md#L17-L41) 本项目应吸收“资源是显式依赖”的思路：工程保存 `instrument_id` 和 asset manifest，渲染进入 `renders/`，MIDI 导出进入 `exports/`，proposal/review 进入 `history/`；许可和署名随资源包保存。

## 不应照搬

### 审美硬规则

上游要求永远不要让同时发声的音符相差一个半音，并要求低音只弹根音或五度、声部跨八度分开。[`SKILL.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/SKILL.md#L15-L23) 这些只能是特定风格的提示，不能成为本项目的阻断条件：钢琴需要悬挂、倚音、音簇、半音色彩和有意未解决的张力。它们最多生成带位置和后果说明的 advisory；非法数据、越权或不可渲染状态才硬拦截。

### GM 128 乐器路线

仓库把全套 128 个 GM program 作为主要卖点，每条轨道按不同 MIDI channel 分配乐器。[`README.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/README.md#L46-L62) [`generate_midi.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/skills/generate_midi.py#L105-L145) 这与当前“先把钢琴做专精”的目标冲突，也会把注意力从触键、踏板、音板和钢琴写作转移到乐器数量。

未来多乐器只需保留 `InstrumentRack`/`Instrument`/`InstrumentSession` seam；当前允许集合默认只有实际可渲染的 `piano`，不能接受任意 GM 名称后静默降级。

### 静默修正和模糊降级

`normalize_composition` 会把非字典输入变成空对象、把 BPM 夹在 20–300、跳过缺字段音符和空轨道。[`normalize_composition.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/skills/normalize_composition.py#L16-L85) `resolve_instrument` 对未知名称模糊匹配，找不到时返回钢琴 program 0。[`gm_instruments.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/midi_types/gm_instruments.py#L201-L228)

这会让模型误以为请求成功。Rust normalize/validate 应返回逐项 findings，拒绝未知 instrument；任何自动归一化都要可见、可复核，而不是静默丢数据。

### 用重复满足最小数量

`refine_composition` 在音符不足时循环复制已有音符，`extend_composition` 也通过重复材料填满目标小节数。[`refine_composition.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/skills/refine_composition.py#L16-L93) 这是典型的反偷懒失败：提高了计数，却没有保证结构、动机发展或听觉内容，还可能破坏段落边界。

本项目应继续用 required-objective coverage、真实 material change、操作预算、事务和渲染后批评防空转；不能用最低音符数、最低轨道数或最低时长衡量质量。沉默和重复本身可以是有意的。

### Python/FluidSynth 作为核心依赖

上游要求运行时 `pip install midiutil`，WAV 转换依赖外部 FluidSynth 和下载的 A320U.sf2。[`SKILL.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/SKILL.md#L48-L69) 这适合轻量 skill demo，不适合 Rust/Tauri 桌面核心：依赖未安装、版本漂移、外部进程失败和资源许可都会影响可复现性。

FluidSynth 可作为可选对照后端；默认渲染继续走 Rust `Instrument`/`InstrumentSession`，资源在控制线程经 `AssetPack` 校验和加载，音频线程不做文件 I/O。

## 上游数据模型与工作流缺口

上游 `Note` 只有 `pitch` 和 `duration`，`Track` 只有顺序音符和 instrument，`Composition` 只有 title、BPM 和 tracks。[`music.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/midi_types/music.py#L9-L28)

- **没有绝对起点。** 生成器用一个 `time` 变量顺序累加 duration，无法表达休止、弱起、跨小节延音或一条轨道内的并行复音。[`generate_midi.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/skills/generate_midi.py#L146-L164)
- **没有表现事件。** 没有 velocity、释放/连奏、CC64、CC67、踏板深度或人性化时序；velocity 固定为 100。[`generate_midi.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/skills/generate_midi.py#L146-L161)
- **没有时间语境。** 只有单一 BPM，没有 tempo map、拍号、PPQ、bar/beat 坐标或量化策略。知识文件里的拍号只是文字，生成器无法验证。
- **没有可审计身份。** 没有 revision、track/clip/note ID、范围、保护区、事务、撤销或并发冲突；按 title 写固定 `output/` 还容易覆盖版本。[`generate_midi.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/skills/generate_midi.py#L167-L179)
- **没有提案—试听—批评闭环。** README 只描述检测请求、调用脚本和可选 WAV 转换；没有 required objective、授权、证据锚点、过期检查或有界修订状态。[`README.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/README.md#L97-L118)

其知识文件也不是可执行的钢琴知识。例如 voice-leading 参考把“避免半音”和传统声部规则写成普遍检查清单，却没有把触键层级、踏板清晰度、音板共鸣、手型风险和音域衰减联系到渲染结果。[`voice-leading.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/resources/voice-leading.md#L1-L35)

## 对应决策

| 参考项目 | 本项目对应物 | 决策 |
| --- | --- | --- |
| 结构化 `Composition` JSON | Rust `Project` + 无损 AI 视图 | 保留结构化，拒绝顺序音符降级 |
| `normalize → generate → convert` | `context/events → review → apply → render/play` | 保留阶段化，Rust 是唯一真源 |
| 按需 theory resources | 钢琴/节奏/曲式 references | 采用，并标来源和适用范围 |
| 固定 `output/` | 工程包 `exports/renders/history` | 采用显式边界，避免覆盖 |
| GM aliases | `InstrumentRack` 实际 catalog | 仅保留可读命名，未知 ID 报错 |
| 重复音符 `refine` | evidence + material change + critique | 拒绝 |
| 半音/根五度硬规则 | 可定位的钢琴 advisory | 拒绝硬拦截 |

## 钢琴专精建议

1. **权威格式继续用 tick IR。** AI 视图增加 `bar.beat.subdivision`、音名和声部角色，但无损保留 ID、绝对/局部 start、duration、pitch、velocity 与 control events。
2. **把钢琴表现作为一等计划信息。** 记录旋律、低音、内声、伴奏、共鸣场等角色，力度曲线、起音/释放、CC64/CC67 意图和 `playability_risk`。这些是 advisory；MIDI 值、资源存在性和授权是硬验证。
3. **节奏绑定当前工程。** 从 PPQ、tempo map 和拍号计算小节/网格。默认允许弱起、切分、三连音、跨小节延音、rubato 和非传统拍号；量化或小节边界只有被用户/宿主明确要求时才限制。
4. **批评必须可定位。** 结构使用 `location + observation + consequence + proposed_revision`，禁止用“更有情感”“更丰富”代替试听证据。
5. **反偷懒不等于强制复杂。** 继续阻断 plan-only、no-op、越权、假 evidence 和旧 revision；稀疏、重复、静态或强烈不协和但有明确意图的音乐只给 advisory。
6. **循环由宿主收敛。** 每轮使用唯一 critique ID，并设置循环上限/超时；成功提交后渲染目标窗口和完整曲目，保存 proposal、review、render metadata，下一轮必须基于新 revision 和新授权。
7. **音色先专精。** 优先打磨现有 `PianoSynth` 的触键、非谐泛音、弦数/微失谐、踏板、共鸣和音板；未来 SFZ/多层采样仍藏在 instrument/asset seam 后，不改变 AI proposal 和权限模型。

## 建议实施顺序

1. 给 `musicctl` 增加带小节/音名/力度/踏板的无损 AI 事件视图，并让 normalize/validate 返回显式 findings。
2. 按上游 progressive-disclosure 思路整理现有钢琴 references；每条建议注明来源、适用风格和是否允许违反。
3. 把 render/play、critique、revision 写入工程历史；循环次数只作为宿主策略，不作为音乐质量分数。
4. 增加 advisory 级的音域密度、踏板混浊、动态层级和可演奏性分析；仍只硬拦截不可表示、不可渲染或越权状态。
5. 最后做高质量钢琴采样器对照，不扩张 GM 乐器表。

## 核心一手来源

- [`SKILL.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/SKILL.md)
- [`midi_types/music.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/midi_types/music.py)
- [`skills/normalize_composition.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/skills/normalize_composition.py)
- [`skills/generate_midi.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/skills/generate_midi.py)
- [`skills/refine_composition.py`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/skills/refine_composition.py)
- [`README.md`](https://github.com/tubone24/midi-agent-skill/blob/19625c682a18e2c7471890c4cdf8568e291b1765/README.md)

# AI 钢琴作曲知识：可执行建议与硬性边界

调研日期：2026-08-19。

本笔记面向项目未来的作曲 skill。它不把西方调性音乐的常见写法冒充“正确答案”，而是把知识分为两层：程序只阻止无效、越权或不可验证的操作；节奏、曲式、配器与表现法作为可解释、可违反的创作建议。来源以大学开放教材、MIDI 官方资料、钢琴家原著、同行评审研究和专业软件官方手册为主。

## 结论：什么应该硬拦截

只有能够客观判定的事项适合成为确定性规则：

- 工程 revision、编辑范围、受保护区域、操作预算和乐器授权必须吻合；proposal 必须实际产生被授权的变化，并以音轨、段落和 tick 范围提供可核验的 objective evidence。
- 音符、控制器和时间事件必须能被当前 IR 与乐器后端表示；时长必须为正，引用的音轨/clip/乐器必须存在，渲染结果不得包含非有限采样值。
- 若采用 MIDI 1.0 控制器语义，CC64 是延音踏板、CC66 是 sostenuto、CC67 是 soft pedal；开关型踏板值 `0..63` 为 off、`64..127` 为 on。CC120、121、123 分别是 All Sound Off、Reset All Controllers、All Notes Off。[S5]

以下内容不应硬拦截：四小节乐句、功能和声、固定和弦密度、必须有高潮、最低音符数、固定左右手跨度、禁止平行音程、必须量化到网格等。它们可以产生 review advisory，但只要创作者说明意图，沉默、重复、不对称乐句、跨小节重音、音簇或稀疏织体都应被允许。

## 节拍、节奏与乐句

### 可执行建议

1. 在 plan 中明确 `pulse`、拍号解释和主要 subdivision。简单拍通常把拍分成二，复合拍通常把拍分成三；例如 `6/8` 的常规感知是两个附点四分音符拍，而不是六个同等强度的拍。[S1]
2. 分开设计三层时间结构：
   - **脉冲与重音**：听者靠什么感到拍点和小节；
   - **表层节奏**：旋律、伴奏分别使用哪些主要时值和切分；
   - **和声节奏**：和弦多久改变一次。和声节奏可以在表层音符不变时加快或减慢，因此应单独记录。[S1]
3. 给每个乐句标出 `entry → continuation → arrival/release`，并指出到达感来自休止、长音、音域、织体、和声还是力度。四或八小节很常见，但开放教材也明确列出三、五、六、七小节等乐句；长度只能作为先验，不能作为验收条件。[S3]
4. 先检查“听感上的拍”是否清楚，再决定量化强度。强拍错位、跨小节延音和不规则分组可以是有意的；review 应询问其作用，而不是自动修正。
5. 需要推进感时，可缩短主要时值、加快和声节奏或提高事件密度；需要展开/停驻感时可反向处理。一次只改变一两个轴，通常更容易听出因果。这是基于教材对 rhythm、texture、dynamics、register 等对比轴的工程化归纳，不是普遍美学定律。[S3]

### AI 自检问题

- 每个段落的拍感、主要 subdivision 和和声节奏能否各用一句话描述？
- 重要音是否总被机械地放在强拍；若是，是否需要切分、延迟或提前来制造方向？
- 乐句结尾真的有到达/悬置的听感，还是仅仅因为 tick 范围结束？
- 节奏变化是否服务于段落功能，而不是为了增加音符数？

## 动机发展与曲式

开放教材把 motive 定义为最小的可辨识旋律思想，并展示从较长主题中抽取动机再发展。可用的变形包括反向进行、音程改变、增值、减值、局部节奏改变、装饰、延展、逆行，以及常见的移调/模进。[S2]

建议为核心动机保存一个简短 identity 描述，例如“短短长节奏 + 上行小三度 + 重音落在末音”，然后在每次出现时记录：

- 保留了哪些身份特征；
- 改变了音高、轮廓、节奏、长度、和声语境、音域或织体中的哪些轴；
- 这次变化承担记忆、推进、对比、过渡还是收束功能。

曲式模板应作为生成支架：

- **sentence** 可采用“基本想法 → 重复/模进 → continuation → cadence”；
- **period** 可采用“较不终止的 antecedent → 较终止的 consequent”；
- 段落对比可以来自 melody、harmony、rhythm、timbre、texture、articulation、dynamics、register 中任意一项或多项。[S3]

这些模板都不是硬规则。AI 可以写不闭合、连续生成、静态或故意反高潮的音乐，但 plan 应明确其组织原则。反偷懒检查也不应要求“每段都不同”，而应要求 AI 指出重复为什么仍然有效，或变化在哪里发生。

## 钢琴写作、声部与踏板

### 声部和织体

- 先给同时发生的材料分配听觉角色：foreground melody、inner voice、bass、harmonic fill 或 resonance。旋律与伴奏的层级必须能从 velocity、register、时值、起音密度和踏板共同听出来。Hofmann 特别警告伴奏淹没旋律、节奏和力度失控会破坏旋律与辅助声部的有机关系。[S4]
- 钢琴可以把旋律、低音和和声压在一件乐器中，但“同时按得下”不等于“单人可演奏”。对大跨度和弦、快速换位、持续声部与密集重复音应标记 `playability_risk`，然后选择重新分配两手、滚奏/琶音、用踏板延续、删减内声或明确声明为机器演奏。
- 不设置统一的“最大九度/十度”硬阈值。Hofmann 明确指出手型差异使通用指法不可行，也警告强迫伸展可能导致伤害；因此固定跨度只能是保守提示，最终应由目标演奏者档案或实际试奏决定。[S4]
- 教材列出的琶音伴奏、低音八度和 Alberti bass 是可调用的钢琴语汇，不是默认填充模板。AI 应说明为何该型态与拍感、和声节奏及旋律留白相容。[S3]

### 力度、连奏与踏板

- 力度应设计成相对层级和跨乐句曲线，避免把所有声部都推到相近 velocity。钢琴的动态上限受具体乐器、触键和声学环境影响；连续堆高并不会无限增加表现力。[S4]
- 指连奏可用相邻音少量重叠近似，但重叠不能造成非预期复音或和声污浊。踏板是延长和着色手段，不应成为掩盖错误声部连接或糟糕 voicing 的工具。[S4]
- 延音踏板的实用起点是：音符起音后再落踏板，在和声或需要清晰分隔的位置换踏板；随后必须以实际渲染听感判断。Hofmann同时强调“和声清晰只是基础”，有意混合非和声音也可以成为色彩，因此不能把“每次和声变化必换踏板”写成硬规则。[S4]
- skill 应同时生成踏板事件和踏板理由，例如 `sustain_bass`、`connect_legato`、`resonance_color`、`accent_release`。只有 MIDI 数值语义由程序硬验；踏板位置和深浅属于听觉判断。[S4][S5]

## 生成—试听—批评—修订闭环

真实作曲过程不是一次性从 plan 线性写到完成。Collins 对一位作曲家三年的实时个案研究使用 MIDI 版本、音频、即时回顾与验证访谈追踪过程，得到的是微观/宏观策略交错、问题增生与解法递归实施的模型，而非单向流水线。[S6]

建议 skill 采用以下有界循环：

1. **冻结上下文**：读取当前 revision、目标窗口和相邻段落，保存可回退版本。
2. **提出一个可听假设**：例如“让第二乐句通过更快的和声节奏和动机减值增强推进”，而不是“让它更好”。
3. **做最小但完整的 patch**：只编辑授权范围，同时保留 objective 与具体音轨/section/range 的 evidence。
4. **用目标钢琴音源渲染**：同时导出全曲目标窗口；必要时再导出 melody、bass/inner voices 等对照 stem。官方 DAW 手册也把 post-fader 主输出描述为“实际听到什么就渲染什么”，并支持逐轨等长导出。[S7]
5. **至少三遍试听**：
   - 不看音符，听整体方向、记忆点、段落比例和意外；
   - 听声部层级、低音、踏板混浊、动态与音域；
   - 对照 brief，核查每条 required objective 的听觉证据。Hofmann把认真聆听自己的输出视为音乐制作和踏板技术的基础。[S4]
6. **把批评写成证据**：使用“在 section B 的第 2–3 小节，伴奏起音与旋律同 velocity，使主题被遮蔽”这种 `location + observation + consequence`，避免“缺少灵魂”“太简单”等不可操作评价。
7. **一次修主要问题**：生成下一 revision，重新 review、render、listen。官方 DAW 工作流保留 Undo history、Save a Copy 和同一项目中的多个版本，适合作为本项目 revision/undo 机制的产品参照。[S7]
8. **停止条件**：所有硬规则和 required objectives 已通过，且最新批评没有指出一个位置明确、收益明确的下一步修改。不能用 note count、操作数或循环次数充当质量分数。

为了避免 AI 通过空泛文字“证明”质量，程序只验证 evidence 是否对应真实受影响范围；音乐是否更有张力、是否足够简洁等判断保留为 advisory，并要求经过渲染试听后再回答。

## 建议进入 skill 的结构化字段

```text
rhythm:
  pulse, meter_interpretation, primary_subdivision, harmonic_rhythm
motifs[]:
  id, identity, transformations[], occurrences[]
sections[]:
  function, range, arrival_type, contrast_axes[]
piano_writing:
  voice_roles[], dynamic_contour, pedal_intents[], playability_risks[]
critique[]:
  location, observation, consequence, proposed_revision
```

这些字段帮助 AI 形成可检查的思考轨迹，但不规定作品必须复杂。允许字段明确写 `intentionally_static`、`intentional_silence` 或 `machine_performance`；关键是意图、实现与听觉结果一致。

## 来源

以下链接均访问于 2026-08-19。

- **[S1] University of Puget Sound，Robert Hutchinson，*Music Theory for the 21st-Century Classroom***： [Meter](https://musictheory.pugetsound.edu/mt21c/meter.html)、[Harmonic Rhythm](https://musictheory.pugetsound.edu/mt21c/HarmonicRhythm.html)。大学维护的开放教材，用于拍、简单/复合拍和和声节奏定义。
- **[S2] 同上**： [Motive](https://musictheory.pugetsound.edu/mt21c/MotiveSection.html)、[Melodic Alteration](https://musictheory.pugetsound.edu/mt21c/MelodicAlteration.html)。用于动机定义及变形方法。
- **[S3] 同上**： [Phrase](https://musictheory.pugetsound.edu/mt21c/PhraseSection.html)、[The Sentence](https://musictheory.pugetsound.edu/mt21c/SentenceStructure.html)、[The Period](https://musictheory.pugetsound.edu/mt21c/PeriodForm.html)、[The Elements of Music](https://musictheory.pugetsound.edu/mt21c/The-Elements-of-Music.html)、[Texture](https://musictheory.pugetsound.edu/mt21c/Texture.html)、[Arpeggiated Accompaniments](https://musictheory.pugetsound.edu/mt21c/ArpeggiatedAccompaniments.html)。用于乐句、曲式、对比轴和伴奏语汇。
- **[S4] Josef Hofmann，*Piano Playing: With Piano Questions Answered***，Project Gutenberg 原著全文：<https://www.gutenberg.org/files/39211/39211-h/39211-h.htm>。重点参考 *The Piano and Its Player*、*Correct Touch and Technic*、*The Use of the Pedal* 和问答中的 stretching/pedal 章节。
- **[S5] MIDI Association，MIDI 1.0 Control Change Messages 官方表**：<https://midi.org/midi-1-0-control-change-messages>。
- **[S6] David Collins, “A synthesis process model of creative thinking in music composition,” *Psychology of Music* 33(2), 2005, DOI**：<https://doi.org/10.1177/0305735605050651>。结论依据出版方登记的摘要与论文元数据。
- **[S7] Ableton Live 12 官方参考手册**： [Recording New Clips](https://www.ableton.com/en/manual/recording-new-clips/)、[Managing Files and Sets](https://www.ableton.com/en/manual/managing-files-and-sets/)。用于循环录制/overdub、撤销、版本保存和实际输出渲染工作流。

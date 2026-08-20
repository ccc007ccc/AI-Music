# AI Music

[![CI](https://github.com/ccc007ccc/AI-Music/actions/workflows/ci.yml/badge.svg)](https://github.com/ccc007ccc/AI-Music/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

一个 MIDI-first 的 Rust 音乐创作系统。用户只需用自然语言说明要创作或调整什么，Autopilot 会在内部自动完成意图理解、创作、Rust 审查、提交、渲染、独立评审与必要修订；task、proposal、critique JSON 只保留为专家级底层接口。

这里的“纯 Rust”特指编排核心与钢琴音频引擎：项目/MIDI/审查、物理近似钢琴、RustySynth SF2 适配、SFZ 预处理、WAV/FLAC 解码、voice/cache、混音和导出都在 Rust 中，仓库没有 C/C++ 源文件或 sampler FFI，workspace 自有 crate 禁止 `unsafe`。Tauri 界面仍是 HTML/CSS/JavaScript，桌面程序通过 CPAL 和系统 WebView 接入 ALSA/PipeWire、WebKitGTK 等平台能力，因此“Rust-native 音频核心”不等于最终二进制不使用任何系统原生库。边界决定见 [`docs/adr/0008-keep-the-piano-engine-rust-native.md`](docs/adr/0008-keep-the-piano-engine-rust-native.md)。

## 当前结构

- `crates/music-core`：项目、音轨、片段、音符、节拍、事务命令和撤销/重做。
- `crates/composition-engine`：创作意图、编辑授权、只读编排观察、提案证据和确定性审查。
- `crates/autopilot-engine`：自然语言入口、Director/Composer/Evaluator 隔离调用、自动重试/修订、影子工程回滚和连续会话记忆。
- `crates/midi-io`：标准 MIDI 文件导入/导出。
- `crates/audio-engine`：有状态乐器会话、纯 Rust 物理近似钢琴、MIDI CC（含延音/软踏板）、混音、WAV 导出和实时播放。
- `crates/project-package`：`.aimusic` 目录工程格式、原子保存、固定资源/导出/渲染/历史目录和路径安全。
- `apps/music-cli`：创建、检查、修改、渲染和播放工程。
- `apps/music-desktop`：Tauri 2 桌面界面，主入口是“AI 自动创作”自然语言输入框。
- `skills/ai-music-composer`：底层专家会话的观察、规划、审查、试听和修订知识。

工程的权威音乐数据是 `.aimusic/project.json` 中的 MIDI 编排，`manifest.json` 只记录工程身份与渲染资源绑定；WAV 和 MIDI 是分别位于 `renders/`、`exports/` 的派生产物。AI、GUI 和 CLI 最终都通过同一个 `ProjectEngine` 提交结构化 `Command`/patch。创作计划不会混进可渲染工程，主观音乐惯例也不会成为隐藏的硬编码限制。旧的单文件 JSON 仍可由 CLI 读取，新建桌面工程始终使用目录包。

目录工程示例：

```text
projects/First Light.aimusic/
├── manifest.json
├── project.json
├── assets/
├── exports/
├── renders/
└── history/
```

创建和检查工程包：

```bash
musicctl new-project projects "My Piano Piece"
musicctl inspect "projects/My Piano Piece.aimusic"
musicctl render "projects/My Piano Piece.aimusic" -o "projects/My Piano Piece.aimusic/renders/preview.wav"
musicctl bind-instrument-pack "projects/My Piano Piece.aimusic" /path/to/piano-pack.json
```

当前示例《初光》位于 [`projects/First Light.aimusic`](projects/First%20Light.aimusic/)；其中 `project.json` 是唯一音乐真源，旧的散落副本已归档到包内对应目录。

仓库还包含一首完整的自动创作示例《晴雨之间》：

- [可编辑工程](projects/%E6%99%B4%E9%9B%A8%E4%B9%8B%E9%97%B4.aimusic/)
- [MIDI 导出](projects/%E6%99%B4%E9%9B%A8%E4%B9%8B%E9%97%B4.aimusic/exports/%E6%99%B4%E9%9B%A8%E4%B9%8B%E9%97%B4.mid)
- [MP3 试听](projects/%E6%99%B4%E9%9B%A8%E4%B9%8B%E9%97%B4.aimusic/renders/%E6%99%B4%E9%9B%A8%E4%B9%8B%E9%97%B4.mp3)
通过 CLI 对目录工程执行成功的 patch 或 proposal 会把对应 revision 的 JSON 记录写入 `history/`，便于 AI 继续批评和修订；桌面端编辑仍由用户保存 `project.json`。

用户主链路是：

```text
用户自然语言指示
  -> Director 生成 Creative Brief
  -> Composer 生成具体 MIDI proposal
  -> Rust 授权、审查并原子提交
  -> render
  -> 独立 Evaluator 接受或要求修改
  -> Composer 强制实现 modify 决定
  -> 最终 WAV、工程、会话记忆和 outcome
```

命令行不需要准备任何 JSON：

```bash
musicctl autopilot "projects/My Piano Piece.aimusic" \
  "创作一个四小节、安静而逐渐明亮的钢琴短句，结尾保持余韵"

musicctl autopilot "projects/My Piano Piece.aimusic" \
  "后两小节更有推动感，但保留安静的开头"
```

第二条指令会读取同一工程的 `history/autopilot-session.json` 和当前 MIDI 事件继续调整。成功结果自动保存到 `project.json`、`history/revision-*-autopilot.json` 与 `renders/autopilot-r*.wav`；目录工程会以回滚式适配器提交写入工程、记忆、outcome 和 WAV，普通磁盘写入失败会恢复旧产物。任一模型调用、审查、提交或渲染失败时，整轮影子工程会回滚，不留下半成品。默认模型适配器复用已登录的 `codex exec` provider 配置，采用只读、无工具、无人工批准的隔离结构化调用；单次模型调用默认 120 秒超时，provider 失败直接返回，内容或 Rust 审查失败才自动重做。

目录工程的音乐保存共用包写锁；有明确基准版本的 CLI、桌面端和 Autopilot 编辑还会比较启动时与落盘时的 revision。如果另一个进程在生成或编辑期间提交了同一工程，本轮结果会被拒绝，不会静默覆盖。这里的“事务”是针对正常 I/O 失败的回滚式适配器提交，不等同于数据库或文件系统级的断电原子事务；准确限制见 [`ROADMAP.md`](ROADMAP.md)。

## 当前成熟度

0.1.0 是可运行的 pre-1.0 开源版本。自然语言到自动创作、评审、修订和保存的主链路已经完成并有本地模拟模型集成测试；真实在线运行仍取决于用户配置的 Codex/provider 可用性。Evaluator 当前依据 MIDI、编排观察和 WAV 数值测量判断，尚未通过音频模型直接听取波形。长曲仍由模型生成完整低层 proposal JSON，复杂编曲的扩展性需要继续改进。完整已知限制与计划见 [`ROADMAP.md`](ROADMAP.md)。

`ProposalReviewer` 只硬性阻止可确定判断的问题：过期 revision、无效 patch、越权轨道/时间范围、受保护区域、未授权删除、不可用乐器、操作预算超限、必须目标无证据、证据和实际 patch 影响不匹配，以及“有操作但最终工程没有任何实质变化”的空转 patch。`CreativeBrief.rhythm` 还可以由宿主显式加入起音网格、小节对齐或最少活跃小节契约；默认为空，不限制 rubato、弱起或稀疏写作。段落数量、音符密度、复杂度、传统和声或固定曲式都不是隐藏通过条件；这些只由 Skill 作为可自由取舍的创作建议。完整架构与行为边界见 [`docs/ai-composition-architecture.md`](docs/ai-composition-architecture.md)。

需要检查作品结构时可以调用只读编排观察器：

```bash
musicctl analyze-arrangement "projects/First Light.aimusic"
musicctl schema arrangement-report
```

它只报告可复核事实（例如连续小节共享起音/时值形状、段落边界发生了哪些维度变化、旋律轮廓的大跳和重复的力度/踏板形状），不打分、不判定风格错误，也不提供唯一修法。报告中的 `semantics` 明确标记这些 finding 为 advisory/non-gating；独立的 AI 评审器结合 `CreativeBrief.style_context`、目标和试听结果作出修改判断，编曲模型负责实现判断，不把逐条质量裁决推回创作者。

## 专家级底层接口

普通用户无需阅读或维护以下 JSON。它们用于调试、替换模型适配器和构建其他可信宿主。

底层编辑可以使用 JSON patch，而不是直接重写整个工程：

```json
{
  "base_revision": 0,
  "description": "在第二小节加入 C 大三和弦",
  "operations": [
    {
      "op": "add_note",
      "track_id": "piano",
      "clip_id": "piano-main",
      "note": {
        "id": "ai-c4",
        "start_tick": 3840,
        "duration_tick": 960,
        "pitch": 60,
        "velocity": 88
      }
    }
  ]
}
```

应用 patch 时，Rust 核心会一次性校验并提交全部操作；任何一项失败都会回滚整个 patch。工程 JSON 会持久化单调递增的 `revision`，AI 应先读取它并填入 `base_revision`，避免覆盖用户刚刚的编辑。

完整创作任务应再加一层 `CompositionTask` 与 `CompositionProposal`，先审查再原子提交：

```bash
musicctl schema composition-task > composition-task.schema.json
musicctl schema composition-proposal > composition-proposal.schema.json
musicctl review-proposal song.json task.json proposal.json
musicctl apply-proposal song.json task.json proposal.json
musicctl session song.json --task task.json
```

`CompositionTask` 把用户意图和编辑权限分开表达；其中 `EditScope` 必须由用户或可信宿主签发/确认，不能让模型通过改 task 给自己扩权。`CompositionProposal` 把可审计计划和实际 patch 配对。每个必须目标都要给出 section、track 或绝对 tick range 锚点，审查器会验证锚点确实和 patch 的受影响区域相交。`apply-proposal` 会在提交时重新审查同一份值，因此 GUI 并发编辑或过期提案不会穿过预检。审查失败时项目文件保持不变。

桌面端现已加入 `CompositionSessions` 可信会话层：宿主先签发并保存不可变任务，模型侧只使用不透明 `task_id` 调用 `review_authorized_proposal` / `apply_authorized_proposal`，无法通过回传修改后的 scope 扩权。提交成功后授权自动消费，撤销后不可再用，新建或加载工程会清除所有旧授权。

CLI 也提供 provider-neutral JSONL 会话：宿主用 `musicctl session song.json --task task.json --role evaluator` 启动评审侧，或用 `--role composer --critique evaluator-report.json` 启动编曲侧。评审侧只能发送 `context`、`analyze`、`events`、`critique`；编曲侧只能读取上下文和已附加的评审结果，再发送 `review`、`apply`、`revoke`、`reload`、`ping`。`critique` 必须带 `brief_id`，并把有位置的观察与独立评审器的逐条 `modify`/`preserve` 决定绑定到当前 revision 与授权范围；一旦宿主附加评审报告，编曲 proposal 必须用 `based_on_critique_id` 逐条回应，不能通过省略链接来选择退出。编曲模型的响应只有观察 ID 和执行说明，不能提交或改写决定；Rust 会验证每个 `modify` 确实在观察的轨道/范围产生实质 patch 影响，而 `preserve` 不强迫无意义改动。默认拒绝 stdin 中的 `authorize`；`--allow-authorize` 仅供可信宿主使用。若工程文件被外部程序改变，会话返回 `project_changed`、重载工程并使全部 task ID 失效，避免旧内存状态覆盖磁盘。协议见 [`docs/cli-session-protocol.md`](docs/cli-session-protocol.md)。

`create_track` 会直接创建一个 `{track_id}-main` 的空 MIDI clip，长度为 16 拍，新轨道立即可以添加音符。轨道还支持 `rename_track`、`remove_track`、`set_track_instrument` 和 `set_track_mixer`。演奏细节可用 `set_note_velocity` 调整力度，并用 `add_control`、`set_control`、`remove_control` 编排 MIDI CC；钢琴当前会处理 CC64 延音踏板（含半踏板）和 CC67 软踏板。GUI 与 AI patch 使用完全相同的命令。

```bash
musicctl apply-patch song.json patch.json
musicctl check-patch song.json patch.json
musicctl review-proposal song.json task.json proposal.json
musicctl apply-proposal song.json task.json proposal.json
musicctl export-midi song.json song.mid
musicctl import-midi song.mid imported.json
musicctl context song.json
musicctl events song.json --track piano --clip piano-main --from 0 --to 3840
```

`context` 输出给 AI 的是精简工程上下文：当前 revision、节拍、轨道/clip ID、事件数量、音域和 mixer，不包含全部音符正文。需要编辑现有内容时，`events` / `clip_window` 按绝对时间窗口返回事件 ID、clip-local/absolute tick，并派生一基小节/拍位、音名、精确四分音符时值比、常见时值标签与钢琴踏板含义。派生字段只帮助模型阅读，数值 tick/pitch/velocity/CC 仍是唯一可编辑数据，非网格时值不会被静默量化。AI 再用 context 中的 revision 作为 patch 的 `base_revision`；`check-patch` / `preview_patch` 会在影子工程中执行完整校验但不写入，返回受影响轨道和预期 revision，通过后再提交同一个 patch。

项目 Skill [`skills/ai-music-composer/SKILL.md`](skills/ai-music-composer/SKILL.md) 定义收敛循环：读取有限上下文、形成 brief/scope、规划、生成提案、反复审查到 `ready`、提交、渲染/试听、带具体诊断修订、最后回读事件确认。Rust 是规则与授权的唯一真源；Skill 使用 CLI 生成的 JSON Schema，不维护另一套易漂移格式。

节拍/乐句、动机与曲式、钢琴声部/踏板/力度、生成—试听—批评—修订流程已经基于大学开放教材、MIDI Association、钢琴家原著、同行评审研究与官方 DAW 手册做了首轮网络调研并凝练进 Skill；完整来源和“硬规则/创作建议”划分见 [`research/composition-knowledge.md`](research/composition-knowledge.md)。

可选的纯 Rust SoundFont 后端基于 MIT 许可的 RustySynth。仓库不内置 SoundFont；使用自己具有授权的 `.sf2`：

```bash
cargo run -p music-cli -- \
  render song.json -o song.wav --soundfont /path/to/piano.sf2
```

也可以用资源包 manifest 管理样本和许可信息。加载时会校验 schema、许可字段、路径和文件类型；`InstrumentRack::from_asset_pack` 根据 engine 选择后端，调用方不需要知道具体格式：

```bash
cargo run -p music-cli -- \
  render song.json -o song.wav --instrument-pack assets/packs/piano.example.json
```

纯 Rust SFZ 采样后端支持 WAV/FLAC、多力度层、音高重采样、起音/释放包络、release sample、CC64 延音和 CC67 软踏板：

```bash
cargo run -p music-cli -- \
  render song.json -o song.wav --instrument-pack assets/packs/piano-sfz.example.json
```

`musicctl` 默认编译两个 Rust 音色后端；`audio-engine` crate 本身仍以 feature 隔离它们，嵌入方可以只选择需要的格式。

资源包的 `instrument_id` 必须和工程 MIDI 轨道的乐器 ID 一致。SFZ 后端在控制线程预处理 `#define`、递归 `#include`、`<master>` 继承和 `<curve>`，并实现钢琴实际需要的 key/velocity 区域、CC 条件与调制、keyswitch 默认层、release sample、`rt_decay`、同音遮罩、`key=-1` 的踏板机械声触发和 WAV/FLAC 预解码。未知播放 opcode、越界样本路径、未定义曲线和不支持的语义会给出行号并拒绝加载，不会静默降级。Salamander Grand Piano V3 的定义文件现在可以完成结构解析；要得到完整音色仍需在工程外下载并登记其授权的样本包，当前实现不承诺与 ARIA 的全部实时扩展逐项等价。

默认钢琴不依赖外部资源：它包含速度相关锤击、1–3 根轻微失谐弦、非谐泛音、分频衰减、延音/软踏板、共鸣弦和全局立体声音板。物理建模思路与许可证边界见 [`research/physical-piano-models.md`](research/physical-piano-models.md)。采样路线调研见 [`research/piano-libraries.md`](research/piano-libraries.md)：首选的完整目标仍是 Salamander Grand Piano V3（CC BY 3.0）；当前后端使用自研严格 SFZ 子集解析器、Claxon 和自研 voice/cache，样本包不直接提交到仓库。

桌面卷帘覆盖 A0–C8 全 88 键，支持双击添加、单击选择、拖动移动、右侧把手调整时值、Delete/Backspace 删除、可选二分/三连音网格、拍号、量化强度、时间标尺和播放光标。量化只作用于当前 clip 的起音，保留时值、力度和踏板控制。轨道区支持新增钢琴轨、双击重命名、乐器选择、音量、声像、静音、独奏和删除。可选乐器来自实际用于播放和导出的 Rust `InstrumentRack`，前端不会展示未注册的音源。所有编辑都会转换为 Rust `Command`，前端不直接修改权威工程状态。

桌面端的“钢琴音色”按钮可选择已授权的 SF2/SFZ 资源包。绑定会写入工程 `manifest.json` 的 `source_assets.instrument:piano`，打开工程时重新校验资源；资源路径失效或许可证字段不完整会明确阻止播放/导出，不会悄悄退回内置钢琴。“另存为”会保留外部绑定，并复制工程内 `assets/`，避免新包悄悄丢失音色。SFZ 工程按当前工程音符预载可达的音高/力度层，避免完整 Salamander 一次性占满内存；音频回调仍只访问预解码缓存。

## 在 dev 容器中验证

```bash
distrobox enter dev -- bash -lc 'cargo test --workspace'
distrobox enter dev -- bash -lc 'cargo run -p music-cli -- demo /tmp/ai-music-demo.json'
distrobox enter dev -- bash -lc 'cargo run -p music-cli -- render /tmp/ai-music-demo.json -o /tmp/ai-music-demo.wav'
distrobox enter dev -- bash -lc 'cargo run -p music-desktop'
```

Tauri 桌面端还需要宿主发行版提供 WebKit/GTK 的开发库；核心和 CLI 不依赖桌面端即可编译测试。

## 开源协作

项目使用 [MIT License](LICENSE)。提交代码前请阅读
[`CONTRIBUTING.md`](CONTRIBUTING.md)；安全问题请按
[`SECURITY.md`](SECURITY.md) 私下报告。版本变化记录在
[`CHANGELOG.md`](CHANGELOG.md)。仓库不包含商业或第三方钢琴样本，用户必须自行提供并遵守相应授权。

# 钢琴合成、采样与播放库调研

调查日期：2026-08-19（GitHub star 数是当天通过 GitHub API 读取的快照，会变化）。

本笔记只引用项目自己的 GitHub README、Cargo/CMake 配置、源码和许可证。目标是给本项目的 Rust 音频引擎找一个可落地的钢琴后端，同时保留以后增加其他乐器的接口。

关键仓库的源码快照（用于避免分支后来移动造成歧义）：

| 仓库 | 检查的 commit（日期） |
|---|---|
| [rustysynth](https://github.com/sinshu/rustysynth/commit/ccfa1a7a34bbc13fd92f6d7f32db08a9be2418b6) | `ccfa1a7`（2026-05-17） |
| [OxiSynth](https://github.com/PolyMeilex/OxiSynth/commit/151b2be8dfcbe9b223d3454f172ffbbc22858808) | `151b2be`（2025-11-16） |
| [FluidSynth](https://github.com/FluidSynth/fluidsynth/commit/db85a59cf8c3feff390957d6d354d01b735a5dac) | `db85a59`（2026-08-11） |
| [sfizz](https://github.com/sfztools/sfizz/commit/f5c6e29f23b8057867c08e88f5f6ac6738baa30b) | `f5c6e29`（2025-03-17） |
| [SalamanderGrandPiano](https://github.com/sfzinstruments/SalamanderGrandPiano/commit/3382bf9496bba2486f5ab0de55a264d1dfc38404) | `3382bf9`（2022-01-03） |
| [soakyaudio/sampler](https://github.com/soakyaudio/sampler/commit/a97f7f6979a1f1025907ccc5fbd52b00ac26219a) | `a97f7f6`（2022-09-30） |
| [SoFiZa](https://github.com/andamira/sofiza/commit/7f4a391f2c735743f1d92dc8513bed329ddb0b66) | `7f4a391`（2022-09-30） |
| [Claxon](https://github.com/ruuda/claxon/commit/5fcd0c1cf66fd182cd21360d8a630312f27036dd) | `5fcd0c1`（2025-12-03） |

## 先给结论

最适合本项目的路线不是把某个完整 DAW 搬进来，而是定义自己的 `InstrumentBackend`/`Voice` 接口，再按阶段替换后端：

1. **先让 Rust 原型出声：** 用 [RustySynth](https://github.com/sinshu/rustysynth)（227 stars，MIT）或 [OxiSynth](https://github.com/PolyMeilex/OxiSynth)（102 stars，LGPL-2.1）播放一个 SF2。两者都已经有 MIDI 事件、实时/离线渲染所需的核心结构；RustySynth 的 README 明确写明支持实时和离线合成，且除标准库外无依赖；OxiSynth 的 README 明确写明纯安全 Rust、WASM 目标，仓库还提供实时 `cpal` 示例。
2. **要获得真正像钢琴的音色：** 优先采用 [Salamander Grand Piano V3](https://github.com/sfzinstruments/SalamanderGrandPiano) 样本（CC BY 3.0），而不是只用简单波形。它是 48 kHz/24 bit、16 个力度层、每三个半音采一个键，并包含击弦释放声和弦共振层；但它是 **SFZ v2 + ARIA 扩展 + FLAC**，不能直接交给只支持 SF2 的 RustySynth/OxiSynth。
3. **纯 Rust 的推荐实现：** 以 [SoFiZa](https://github.com/andamira/sofiza)（14 stars，MIT/Apache-2.0）作为 SFZ 数据结构和 opcode 类型参考，以 [Claxon](https://github.com/ruuda/claxon)（331 stars，Apache-2.0）解码 FLAC，再参考 [soakyaudio/sampler](https://github.com/soakyaudio/sampler)（19 stars，MIT）的 `Sound`/`Voice`/voice-stealing/实时处理拆分。代码核对发现 SoFiZa 虽把 Salamander 列为目标库，但当前 `Instrument::from_file` 不展开 `#include`，解析状态也没有实现 `<master>` 继承，不能据此宣称已经兼容 Salamander。当前实现因此先使用会拒绝未知语义的自研严格子集解析器。
4. **C/C++ 项目只作行为对照：** [sfizz](https://github.com/sfztools/sfizz)（533 stars，BSD-2-Clause）有成熟的 SFZ 加载、事件和 block render 设计，适合核对术语、线程边界和离线听感；本项目不链接它，也不增加 C FFI 后端。Salamander 自己声明依赖大量 SFZ 2/ARIA 扩展，仍须逐项验证 opcode，不能凭“能加载 SFZ”就承诺完全兼容。
5. **FluidSynth**（2464 stars，LGPL-2.1）同样只作为成熟 SF2 实现的研究对照或开发者外部工具，不进入产品依赖。钢琴后端保持 Rust-native，避免 CMake、动态库与额外 FFI/许可证边界。

因此建议的长期分层是：

```text
Music IR / MIDI / 时间轴
          ↓
InstrumentBackend trait
   ├── RustSampler (strict SFZ subset + Claxon + 自己的 Voice)
   └── RustySynth/OxiSynth (SF2 快速预览)
          ↓
        Mixer → CPAL/WAV
```

## 候选项目比较

| 项目 | 语言/协议 | 许可 | GitHub stars（2026-08-19） | 可复用部分 | Linux/集成判断 |
|---|---|---:|---:|---|---|
| [FluidSynth](https://github.com/FluidSynth/fluidsynth) | C/C++，SoundFont 2 | LGPL-2.1 | 2464 | 成熟的 SF2 voice、效果器、MIDI 文件/事件处理 | 仅作对照/外部工具，不链接进产品 |
| [TinySoundFont](https://github.com/schellingb/TinySoundFont) | 单头文件 C/C++，SF2 | MIT | 863 | 极小的 SF2 播放核心 | 仅作算法比较，不复制或 FFI 集成 |
| [sfizz](https://github.com/sfztools/sfizz) | C++，SFZ | BSD-2-Clause | 533 | SFZ parser、采样 voice、实时 block API、C ABI | 仅作语义与听感对照，不链接进产品 |
| [Claxon](https://github.com/ruuda/claxon) | Rust，FLAC decoder | Apache-2.0 | 331 | 纯 Rust FLAC 解码 | 不是乐器，但正好可解 Salamander 的 `.flac` |
| [RustySynth](https://github.com/sinshu/rustysynth) | 纯 Rust，SF2/MIDI | MIT | 227 | SoundFont reader、voice、包渲染、MIDI sequencer、reverb/chorus | 最容易直接嵌入；只支持 SF2，不直接读 Salamander SFZ/FLAC |
| [@tonejs/piano / tambien/Piano](https://github.com/tambien/Piano) | TypeScript/WebAudio，Salamander 样本 | MIT（代码；样本另有来源许可） | 209 | 采样键映射、力度层选择、延音/释放/踏板/共振分层的清晰参考实现 | 不是 Rust runtime；可移植其状态机和映射规则 |
| [SalamanderGrandPiano](https://github.com/sfzinstruments/SalamanderGrandPiano) | SFZ + FLAC 样本 | CC BY 3.0 | 189 | 高质量钢琴样本及 SFZ 区域定义 | 样本包很大；需保留署名，且要处理 ARIA opcode/`#include` |
| [OxiSynth](https://github.com/PolyMeilex/OxiSynth) | 纯安全 Rust，SF2/SF3 可选 | LGPL-2.1 | 102 | `Synth`、`MidiEvent`、实时 `read_next`/离线 `write`、独立 chorus/reverb crate、WASM 设计 | Rust 集成好；LGPL 义务和 0.x API 稳定性需评估 |
| [DDSP-Piano](https://github.com/lrenault/ddsp-piano) | Python/TensorFlow，神经 MIDI→audio | Apache-2.0（代码） | 99 | 可参考物理/神经控制参数、MAESTRO 训练流程 | 适合未来离线实验；不适合当前 Rust 实时 MVP，模型权重/数据要单独审许可 |
| [qiano](https://github.com/claytonotey/qiano) | C++，数字波导物理模型 | GPL-2.0 | 36 | 物理模型与波导思路，含 CLI/VST2.4 | POSIX/Linux 可构建；GPL 和旧 autotools 使其更适合作算法参考，不宜直接嵌入 |
| [Steinway-D-274](https://github.com/tongxunlu/Steinway-D-274) | SF2 样本包 | CC BY-SA 4.0 | 26 | 另一套免费三角钢琴样本 | README 描述为 16-bit WAV、动态滤波且没有普通多力度层；仓库为多段 RAR、约 1.9 GB，不适合首个内置资源 |
| [soakyaudio/sampler](https://github.com/soakyaudio/sampler) | 纯 Rust，SFZ/WAV sampler | MIT | 19 | 泛型 `SamplerSound`/`SamplerVoice`、ADSR、音域/力度层、voice stealing、CPAL/midir/ringbuf | 最有价值的 Rust 结构参考；README 明确说当前主要在 macOS 开发，Linux 需要验收 |
| [fan455/fan455_piano_synthesis](https://github.com/fan455/fan455_piano_synthesis) | Rust，有限元物理模型 | MIT | 17 | 1D/3D 弦、锤、音板和声学传播的研究代码 | README 明确称项目仍在开发、目前可能无法运行，并依赖 GMSH/Intel MKL；不能作为 MVP 依赖 |
| [SoFiZa](https://github.com/andamira/sofiza) | Rust，SFZ parser | MIT/Apache-2.0 | 14 | SFZ token/opcode/region/global/group 模型；README 列出 Salamander 兼容目标 | 解析器可直接复用或作为参考；需要自行处理预处理、样本缓存和渲染 |
| [oxideav-midi](https://github.com/OxideAV/oxideav-midi) | Rust，SMF + SF2/SFZ/DLS scaffold | MIT | 0 | 很新的纯 Rust MIDI 时间线、tick→秒、SF2/SFZ/DLS voice scaffold | 功能方向很接近本项目，但版本 0.0.4、star 很少；应先做代码/测试审计，不宜作为唯一基础 |

## 重点项目的原始证据与适用边界

### RustySynth：第一版最省事的 Rust SF2 后端

- [README](https://github.com/sinshu/rustysynth/blob/main/README.md) 写明它是纯 Rust SoundFont MIDI synthesizer，适合实时和离线合成，支持标准 MIDI 文件和动态速度变化，并且除标准库外没有依赖。
- README 的示例直接展示 `SoundFont::new`、`Synthesizer::new`、`note_on` 和 `render`；同一页也给出 MIDI sequencer 和实时音频示例。
- [Cargo.toml](https://github.com/sinshu/rustysynth/blob/main/rustysynth/Cargo.toml) 标注 `license = "MIT"`、版本 `1.3.6`；[LICENSE.txt](https://github.com/sinshu/rustysynth/blob/main/LICENSE.txt) 是 MIT。
- [synthesizer.rs](https://github.com/sinshu/rustysynth/blob/main/rustysynth/src/synthesizer.rs) 展示了 16 MIDI channel、note on/off、CC、pitch bend、hold pedal、reverb/chorus 等处理。

它的边界是格式：RustySynth 读取 SF2，不是 SFZ。若要直接用 Salamander，需先有合法的 SFZ→SF2 转换流程，或者把 Salamander 作为另一种后端；不要把转换后的大二进制样本直接提交到本项目仓库。

### OxiSynth：更现代的纯 Rust/WASM 方向

- [README](https://github.com/PolyMeilex/OxiSynth/blob/master/README.md) 称其为 pure safe Rust SoundFont synthesizer，最初为 Neothesia 集成，随后用于微分音和 Black MIDI；同页明确展示 WASM 浏览器运行。
- [Cargo.toml](https://github.com/PolyMeilex/OxiSynth/blob/master/oxisynth/Cargo.toml) 标注 LGPL-2.1；`sf3` 是可选 feature，chorus/reverb 是独立 workspace crate。
- [simple.rs](https://github.com/PolyMeilex/OxiSynth/blob/master/oxisynth/examples/simple.rs) 展示 `Synth::default`、`SoundFont::load`、`add_font`、`send_event`、`write`；[实时示例](https://github.com/PolyMeilex/OxiSynth/blob/master/oxisynth/examples/real-time/src/main.rs) 展示 `cpal` 输出和 MIDI 事件通道。

若项目将来需要 Tauri 前端中的 WASM/跨平台预览，OxiSynth 比 C FFI 更顺手；但 LGPL 和当前 0.x 版本应在架构上隔离为可替换实现。

### sfizz：SFZ 行为对照，不作为产品后端

- [README](https://github.com/sfztools/sfizz/blob/develop/README.md) 定义它为 SFZ parser and synth C++ library，并说明可作为自己程序中的库；Linux 构建和 JACK standalone 路径均有说明。
- [许可证](https://github.com/sfztools/sfizz/blob/develop/LICENSE) 是 BSD 2-Clause。README 同时列出默认 `dr_libs` 音频库、可选 `libsndfile` 以及各第三方许可。
- [公开 C API](https://github.com/sfztools/sfizz/blob/develop/src/sfizz.h) 明确规定 RT（实时）和 CT（控制）线程约束，提供 `sfizz_load_file`、`sfizz_send_note_on/off`、CC、sample-rate/block-size 设置和 `sfizz_render_block`。
- [CMakeLists.txt](https://github.com/sfztools/sfizz/blob/develop/CMakeLists.txt) 显示 C++/C 构建、Linux 默认 JACK 选项、可选 renderer 和 shared library。

这些材料用于核对控制线程/实时线程划分和 SFZ 行为，不授权把其 C/C++ 实现或 C ABI 引入本项目。Salamander README 明确要求 SFZ 2.0 + ARIA 扩展并提示非 ARIA sampler 可能出现问题，因此实际方案仍要用测试曲目验证 `trigger=release`、`off_time`、`sw_*`、`ampeg_*`、`locc/hicc` 和 `#include`，不能默认任何非 ARIA sampler 完全等价。

### Salamander Grand Piano：最值得保留的样本资产

- [README](https://github.com/sfzinstruments/SalamanderGrandPiano/blob/master/README.md) 给出录音规格：48 kHz/24 bit、16 velocity layers、从最低 A 起每三个半音采样、按键击打释放声 chromatic 一层、弦共振每三个半音三层、AKG C414 AB 话筒；并说明是 SFZ v2 + ARIA extensions、FLAC。
- 同一 README 的 [License](https://github.com/sfzinstruments/SalamanderGrandPiano/blob/master/README.md#license) 指向 CC BY 3.0；仓库 [LICENSE](https://github.com/sfzinstruments/SalamanderGrandPiano/blob/master/LICENSE) 包含完整 CC 法律文本。
- [SFZ 文件](https://github.com/sfzinstruments/SalamanderGrandPiano/blob/master/Salamander%20Grand%20Piano%20V3.sfz) 记录了速度响应、延音、release、keyswitch、string resonance/hammer/pedal 层等具体映射；它还使用 `#include "Data/*.txt"` 和 `Samples/*.flac`。

建议把样本包当成用户可下载的 asset pack：工程文件只保存 asset ID、版本、路径和许可证/署名信息；不要默认把几百 MB 甚至更大的样本复制到 Git 仓库或安装包。

### SoFiZa + soakyaudio/sampler：纯 Rust 自研采样器的参考起点

- [SoFiZa README](https://github.com/andamira/sofiza/blob/master/README.md) 写出目标 opcode 来自若干自由样本库，并列出 **Salamander Grand Piano v3**；这更适合解读为实现目标，不是完整兼容保证。
- [SoFiZa Cargo.toml](https://github.com/andamira/sofiza/blob/master/Cargo.toml) 标注 `MIT/Apache-2.0`；[Instrument](https://github.com/andamira/sofiza/blob/master/src/sfz/instrument.rs) 将 global/group/region/default_path 暴露为结构化数据。
- 同一个 `Instrument` 实现的 `from_file` 只读取单个文本后调用 `from_sfz`；token 循环没有展开 `#include/#define`，且 `<master>` 没有进入 global/group/region 的继承状态。Salamander 主定义依赖这三项，所以“能识别 token”不等于能正确得到区域映射。
- [soakyaudio README](https://github.com/soakyaudio/sampler/blob/main/README.md) 列出实时音频/MIDI、多力度层、SFZ loader、线性 ADSR 和 voice stealing；同时明确当前只支持 WAV、loop/disk streaming 尚在 roadmap，并提示主要在 macOS 开发。
- [泛型 Sampler](https://github.com/soakyaudio/sampler/blob/main/src/processing/sampler.rs) 把 `SamplerSound`、`SamplerVoice`、sustain pedal、voice priority 和 block render 分开；[SFZ loader](https://github.com/soakyaudio/sampler/blob/main/src/format/sfz_loader.rs) 演示如何将 SFZ region 的 key/velocity/root/attack/release 映射到 sample voice；[AudioFileVoice](https://github.com/soakyaudio/sampler/blob/main/src/processing/sampler/audio_file_voice.rs) 演示变调、线性插值和释放包络。

这正好对应我们的深模块边界：

```text
SFZ parser/asset manifest → Region/Layer
FLAC/WAV decoder/cache    → immutable SampleData
VoiceAllocator            → key/velocity/pedal/stealing
InstrumentBackend         → render_block(events, out)
```

第一版只需实现钢琴所需的有限 opcode，不要一开始承诺完整 SFZ/ARIA。当前 [`sfz_piano.rs`](../crates/audio-engine/src/sfz_piano.rs) 已实现严格的 `#define`/`#include` 预处理、`<master>`/`<group>` 继承、WAV/FLAC 预解码缓存、多力度区域、线性变调、包络、release sample、CC 条件与主要 ARIA 调制、`rt_decay`、CC64/67、`key=-1` 踏板机械声和 voice stealing；未知播放 opcode、未定义曲线或越界资源会带行号拒绝。它已经能结构解析 Salamander 的主定义并在样本存在时建立区域表，但完整样本库仍需按需下载、授权登记和后续流式策略来控制内存。

### FluidSynth 与 TinySoundFont：SF2 回归后端

- [FluidSynth README](https://github.com/FluidSynth/fluidsynth/blob/master/README.md) 将其定义为跨平台、实时、基于 SoundFont 2 的软件合成器，可读取 MIDI 设备和播放 MIDI 文件；同页列出 LGPL-2.1 和构建文档。
- [CMakeLists.txt](https://github.com/FluidSynth/fluidsynth/blob/master/CMakeLists.txt) 显示 ALSA、JACK、PipeWire、SDL、PulseAudio 等可选音频后端，以及 shared library 选项。
- [TinySoundFont README](https://github.com/schellingb/TinySoundFont/blob/main/README.md) 说明它是单个 C header 的 SF2 synthesizer，示例覆盖 Linux/Windows/macOS，除 C 标准库没有外部依赖；[LICENSE](https://github.com/schellingb/TinySoundFont/blob/main/LICENSE) 是 MIT。
- TinySoundFont README 还包含项目自己的 **Strict No LLM / No AI Policy**。因此即便将其作为未修改依赖，也不应向该上游提交 AI 生成的 patch；本项目不应复制其代码来规避该政策。

两者都不能直接解释 Salamander 的 SFZ/ARIA；它们适合先用一个小 SF2 验证 MIDI 时间轴、渲染和混音的一致性。

### 物理模型与神经模型：后续研究，不阻塞 MVP

- [qiano README](https://github.com/claytonotey/qiano/blob/master/README) 描述数字波导物理模型，并包含 VST2.4、命令行和 MATLAB MEX；[LICENSE](https://github.com/claytonotey/qiano/blob/master/LICENSE) 是 GPL-2.0。它适合作为波导/琴弦模型的算法阅读材料，不建议直接链接进当前 MIT/Apache 方向的核心程序。
- [fan455 README](https://github.com/fan455/fan455_piano_synthesis/blob/main/README.md) 描述 Rust 有限元模型（弦、非线性锤弦、3D 音板、桥耦合和声传播），但也明确说项目仍在 active development、目前可能无法运行，并依赖 GMSH 和 Intel MKL；[Cargo.toml](https://github.com/fan455/fan455_piano_synthesis/blob/main/Cargo.toml) / LICENSE 标注 MIT。可以作为独立研究实验，不应成为第一版播放依赖。
- [DDSP-Piano README](https://github.com/lrenault/ddsp-piano/blob/main/README.md) 提供 MIDI→WAV 命令、MAESTRO 训练/评估流程、模型配置和 checkpoint；[synthesize_midi_file.py](https://github.com/lrenault/ddsp-piano/blob/main/synthesize_midi_file.py) 显示其依赖 TensorFlow/DDSP、加载 checkpoint 后离线写 WAV；代码 [LICENSE](https://github.com/lrenault/ddsp-piano/blob/main/LICENSE) 是 Apache-2.0。它适合以后做离线“AI 钢琴音色”实验，不适合作为当前实时 Rust 引擎。

## 推荐实施顺序

### 阶段 A：先验证编排和音频接口

1. 定义自己的 `InstrumentBackend`：`prepare(sample_rate, block_size)`、`send_event(AudioEvent)`、`render_block(&mut [f32])`、`reset()`。
2. 用 RustySynth 或 OxiSynth 加一个小型、许可清楚的 SF2 做单轨播放和离线 WAV；这一步验证 tick→sample、note-on/off、CC64 sustain、seek、播放/渲染一致性。
3. 保持 `Project/Track/NoteEvent` 与音源格式无关；工程文件只保存 MIDI/编排数据和 `instrument_id`。

### 阶段 B：纯 Rust Salamander sampler

1. 在现有严格解析器前为 Salamander 写一个可测试的预处理器，将 `#define`、递归 `#include Data/*.txt` 和 `<master>` 继承展开；SoFiZa 继续作为 opcode 覆盖参考，而不是兼容性兜底。
2. 用 Claxon 在控制线程解码 FLAC；音频线程只读预加载/缓存的 `Arc<SampleData>`，不做文件 I/O 或分配。
3. 先支持 `sample`、`lokey/hikey`、`lovel/hivel`、`pitch_keycenter`、`tune`、`volume`、`pan`、`ampeg_attack/release`、`trigger=release`、CC64 和有限的 release/resonance 层。
4. 参考 soakyaudio 的 voice allocator，但把样本缓存、循环、round-robin 和 disk streaming 设计成独立模块。
5. 先只加载一个中音区、2–4 个 velocity layers 做回归测试；完整 16 层/全键位作为可下载 asset pack，并按需缓存，避免一次性把全部 FLAC 解码进 RAM。

### 阶段 C：纯 Rust 兼容性与流式加载

当纯 Rust sampler 有可听结果后，继续在 Rust 模块内部补足实际钢琴资源需要的 opcode、分块预取和有界缓存。可在开发环境中用独立的外部 sampler 渲染同一 MIDI 做听感对照，但它不参与产品构建、运行时加载或工程可复现链路。

## 许可与资产注意事项

- **代码许可与样本许可分开记录。** `tambien/Piano` 的 TypeScript 代码是 MIT，但它引用的 Salamander 录音并不因此变成 MIT；Salamander 仓库自己标 CC BY 3.0，发布时要保留作者/来源署名。
- `Steinway-D-274` 是 CC BY-SA 4.0，ShareAlike 条件与本项目未来分发方式可能冲突；它的多段 RAR 也不适合作为默认资源。
- LGPL（FluidSynth/OxiSynth）和 GPL（qiano）需要在发布形态确定后再做法律审查；推荐先把它们放在可替换后端，不让核心 `music-core` 依赖具体许可证。
- 样本包、模型权重和训练数据的许可必须分别核对。DDSP-Piano 的 Apache 代码许可不能自动覆盖 MAESTRO 数据或 checkpoint 的所有分发场景。

## 最终建议

当前项目应继续深化已经落地的 **Rust-native `RustSampler`（严格 SFZ 子集 + Claxon + 自己的 voice/cache）**，并保留纯 Rust SF2 实现作为可替换预览后端。C/C++ sampler 只保留在研究比较表中，不建立 FFI seam。下一步重点是可验证的 Salamander 语义、按需样本缓存和桌面端资产管理，而不是增加更多乐器。

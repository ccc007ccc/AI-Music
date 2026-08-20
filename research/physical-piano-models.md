# 物理钢琴模型：可借鉴算法与许可证边界

调查日期：2026-08-19。

本文只使用项目作者的仓库、源码、许可证和作者维护的技术文档。结论服务于本仓库的纯 Rust 默认钢琴；这里讨论的是可独立实现的 DSP 思路，不代表复制任何参考项目的代码。

## 结论

对当前产品，最合适的默认音色不是一次性照搬完整数字波导，而是一个实时成本可控的混合模型：

```text
速度相关的短锤击激励
  → 每音 1–3 根轻微失谐的非谐模态弦
  → 音高/泛音相关衰减和松键阻尼
  → 踏板控制的共鸣弦
  → 全局立体声音板模态 + 短反馈延迟
```

它保留了物理意义明确的参数，又能藏在现有 `Instrument` / `InstrumentSession` 接缝后面。真正追求录音级拟真时，仍应使用许可清楚的多层采样资产；物理模型与采样器是两个可替换 adapter，而不是互相排斥的工程路线。

## 一手参考与许可

| 来源 | 一手证据 | 可借鉴算法 | 可复制代码 |
|---|---|---|---|
| STK | 官方 [README](https://github.com/thestk/stk/blob/master/README.md)、[LICENSE](https://github.com/thestk/stk/blob/master/LICENSE)、[`Modal`](https://github.com/thestk/stk/blob/master/src/Modal.cpp)、[`BandedWG`](https://github.com/thestk/stk/blob/master/src/BandedWG.cpp) | 激励进入若干共振模态；每个模态有比率、增益和衰减；带状波导以多个延迟/带通回路表现振动模态 | 许可证为宽松的 MIT 风格文本，可以在保留版权与许可文本的条件下复用；本项目目前没有复制其实现，只采用公开 DSP 概念 |
| qiano | 官方 [README](https://github.com/claytonotey/qiano/blob/master/README)、[COPYING](https://github.com/claytonotey/qiano/blob/master/COPYING)、[`qiano.cpp`](https://github.com/claytonotey/qiano/blob/master/src/qiano.cpp) | 数字波导钢琴应考虑锤弦、1/2/3 弦配置、微失谐、弦损耗、音板、桥耦合以及纵向/横向模式 | GPL-2.0；不能把其代码复制或链接进当前 MIT 核心后仍按 MIT 分发。本项目只把参数类别当研究清单，未移植公式、常量或程序结构 |
| CCRMA / Julius O. Smith | 作者维护的 *Physical Audio Signal Processing*：[数字波导](https://ccrma.stanford.edu/~jos/pasp/Digital_Waveguide_Models.html)、[钢琴锤](https://ccrma.stanford.edu/~jos/pasp/Piano_Hammer_Modeling.html)、[刚性钢琴弦](https://ccrma.stanford.edu/~jos/pasp/Stiff_Piano_Strings.html)、[换位钢琴合成](https://ccrma.stanford.edu/~jos/pasp/Commuted_Piano_Synthesis.html)、[耦合弦](https://ccrma.stanford.edu/~jos/pasp/Coupled_Piano_Strings.html) | 双向延迟线/滤波延迟环；锤子可视作质量、弹簧和阻尼；弦刚性会拉伸泛音；同音的两三根弦应稍有失谐；线性音板/箱体可与弦系统换位并预计算为激励响应 | 技术文档是受版权保护的论述，不是本项目代码许可证。数学、物理原理和论文思想可独立实现，但不复制页面文字、图和代码素材 |
| `@tonejs/piano` | 官方 [README](https://github.com/tambien/Piano/blob/master/README.md)、[LICENSE](https://github.com/tambien/Piano/blob/master/LICENSE.md)、[`Strings.ts`](https://github.com/tambien/Piano/blob/master/src/piano/Strings.ts)、[`Pedal.ts`](https://github.com/tambien/Piano/blob/master/src/piano/Pedal.ts)、[`Keybed.ts`](https://github.com/tambien/Piano/blob/master/src/piano/Keybed.ts) | 采样器状态机：力度层选择、活动音符映射、note-on 重触发、踏板状态、松键与踏板机械声分层 | 代码为 MIT，可按许可证复用；Salamander 录音是独立资产许可，不能因播放器是 MIT 就当成 MIT 样本。本项目没有复制 TypeScript 代码 |
| Pianoteq / Modartt | 官方站点只公开其产品是 physically modelled instrument，并未开放合成源码或允许复制实现：[Modartt](https://www.modartt.com/) | 可作为听感和功能基准，公开论文可逐篇阅读其理论结论 | 产品与实现是专有的；不得反编译或声称复刻其源码。当前实现没有使用 Pianoteq 代码、模型或资产 |

访问上述链接日期均为 2026-08-19。

### MDA Piano 的边界

网上存在多个 MDA 插件镜像和移植，但本次没有找到同时具备明确官方归属、钢琴源码和可核验许可证的一手仓库。因此不把 MDA Piano 当作当前实现来源，也不根据非官方镜像复制波表、采样数据或常量。如果后续找到原作者发布包，应先单独核对其中源码和音频数据的许可。

## 算法选择

### 1. 模态弦，而不是完整有限元

每根弦用若干个二阶共振模态近似。第 `n` 个泛音频率采用轻微非谐伸展：

```text
f_n ≈ n · f_0 · sqrt(1 + B · n²)
```

其中 `B` 随音域变化。每个模态独立衰减，上方泛音衰减更快。这样可直接表现钢琴弦刚性和频率相关损耗，又不需要在实时线程求解有限元网格。

### 2. 一至三根弦和微失谐

低音通常用较少弦，中高音用两至三根弦。每根弦偏移不到约一音分，使拍频缓慢变化。CCRMA 的耦合弦章节明确指出钢琴的同音应使用多个略失谐的回路；qiano 源码也把一、二、三弦和 detune 作为模型参数类别。当前实现把它简化为多个独立模态组，后续可再加入显式桥耦合。

### 3. 速度相关锤击

严格的锤弦模型是非线性的，锤毡可近似为带质量、弹簧、阻尼和滞回的接触体。第一版不求解接触微分方程，而使用短促、确定性的带限噪声与低频冲量；力度同时控制：

- 总激励能量；
- 高频含量；
- 泛音谱斜率；
- 软踏板时的弦数与激励强度。

这与换位合成把锤弦接触近似为一个或少数速度相关力脉冲的方向一致，但所有波形、参数和 Rust 实现均为本项目独立设计。

### 4. 阻尼、踏板和共鸣

松键后渐变到更短的 T60，而不是立即截断振荡；CC64 按下时延迟阻尼。额外维护 88 个低成本共鸣振子：被按住的键或延音踏板打开时，如果新音与其泛音接近，就加入少量能量。该方法是 sympathetic resonance 的轻量近似，不需要让每个活动 voice 互相全连接。

### 5. 全局音板

音板属于整台琴而非单个音符，所以在每轨 session 中只有一个全局状态：

- 稀疏、宽频的立体声共振模态；
- 数条带阻尼反馈的短延迟，用于箱体尾音和空间扩散；
- 音锤与踏板产生少量机械激励。

CCRMA 的换位合成说明线性音板、箱体和空间响应可被换到激励侧。以后可把当前算法音板替换为许可清楚的测量脉冲响应，而不改变工程或乐器 interface。

## 当前 Rust 实现的独立性

[`crates/audio-engine/src/piano.rs`](../crates/audio-engine/src/piano.rs) 是新写的 Rust 实现，使用标准振荡器递推、指数 T60、确定性伪随机激励和自研参数。它没有复制 qiano/STK/Tone/Pianoteq 的源代码、数据表或资产。

当前已实现：

- 1–3 弦与微失谐；
- 非谐泛音和每音/每泛音衰减；
- 力度相关锤击与亮度；
- CC64 延音、CC67 软踏板、all-notes-off/reset；
- 踏板/按键控制的共鸣弦；
- 全局立体声音板和确定性机械噪声；
- voice stealing 和无 NaN/Inf 的密集和弦测试。

## 后续优先级

1. 用一组自有或许可清楚的钢琴录音做自动频谱/衰减拟合，不把录音提交进仓库。
2. 为 `PianoSynth` 增加版本化 preset，而不扩大 `InstrumentSession` interface。
3. 加入桥耦合，使同音多弦之间的拍频和双阶段衰减更自然。
4. 把音板改成可选的短脉冲响应卷积 adapter；无资产时继续使用算法音板。
5. 保留 RustySynth/SF2 和现有纯 Rust SFZ sampler 作为采样对照，进行响度、attack、pedal tail 的 A/B 回归。

# Piano writing lenses

Write for the rendered piano as a resonant, decaying, velocity-sensitive instrument. Register, spacing, attack, overlap, and pedal often matter more than nominal chord labels.

## Register and texture

- Low-register close intervals accumulate dense partials quickly; use wider spacing, fewer simultaneous tones, or shorter/pedal-aware durations when clarity matters.
- Middle register carries much of the perceived harmonic body and can mask a melody placed there. Give the primary line distinction through register, timing, velocity, articulation, or texture.
- High-register notes decay and color differently from bass notes. Balance is not achieved by assigning identical velocities to every chord tone.
- Decide the functional texture—single line, melody/accompaniment, contrapuntal voices, chordal mass, ostinato, resonance field, or transitions between them—before filling every beat.

## Voicing and motion

- Voice chords according to perceptual priority. Outer voices and attack timing are especially salient; inner notes can support color without equal emphasis.
- Use common tones, contrary/oblique motion, or stepwise voice leading when continuity is desired. Use registral jumps, revoicing, displaced bass, or changed attack shape when contrast is desired.
- Treat hand-like reach as an expressive realism lens, not a universal ban. Very wide or overlapping sonorities can be intentional, rolled, sustained by pedal, or written as studio-layered material.
- Avoid mechanically duplicating the same chord in both hands unless the massed sonority is the point.

## Articulation, dynamics, and pedal

- Note duration controls key release; pedal controls damper release. Plan both. Long MIDI notes plus full pedal can obscure harmony and pulse.
- CC64 is continuous in this engine, so values can express partial pedal rather than only off/on. Clear or reduce pedal around harmony changes when accumulated resonance conflicts with the brief.
- For external MIDI interoperability, remember that the MIDI 1.0 switch convention treats CC64 values 0–63 as off and 64–127 as on; this engine deliberately uses the full range continuously for half-pedal rendering.
- CC67 soft pedal changes color as well as level in the current piano model. Use it for a deliberate timbral passage, not as a generic volume substitute.
- Shape velocity by phrase and voice. Accents, arrival points, accompaniment balance, and repeated-note direction should be intentional.
- Listen for clipping or weak balance before reaching for mixer changes; first determine whether voicing and velocity are the actual cause.

## Audit questions

- Which voice or layer should the ear follow at each moment?
- Does spacing suit the chosen register and pedal state?
- Are attacks and releases producing the intended articulation?
- Does resonance connect phrases or blur them?
- Is the texture physically suggestive, deliberately impossible, or accidentally awkward?

These are creative lenses, not authorization rules or mandatory simulations of two human hands.

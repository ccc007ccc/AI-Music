# Rhythm and timing lenses

Rhythm is an interaction among pulse, grouping, onset pattern, duration, accent, and change over time. Use these as design variables, not a checklist.

When relevant, record the perceived pulse, meter interpretation, primary subdivision, and harmonic rhythm in the plan summary or creative decisions. In compound meter such as 6/8, do not automatically equate the denominator unit with the perceived beat.

## Establishing and bending expectation

- Give the listener enough repeated timing or accent information to infer a pulse or grouping before relying on syncopation, displacement, or ambiguity—unless ambiguity is the intended opening effect.
- Let phrase rhythm serve the musical function. A stable statement may use recurring onset shapes; a response can compress, extend, displace, fragment, or leave space.
- Distinguish syncopation from random off-grid placement. Syncopation creates tension against an audible metric expectation and gains meaning from what surrounds it.
- Preserve hierarchy across time scales: subdivision, beat, bar, phrase, and section can each carry a different pattern. Avoid changing all levels at once unless rupture is the goal.

## MIDI realization

- Derive tick values from current PPQ and meter. Decide the intended rhythmic value first, then calculate ticks.
- Keep exact grid timing for deliberately mechanical music. For humanized timing, vary events relative to a stable reference and preserve ensemble/hand coordination where attacks are meant to align.
- Shape duration as carefully as onset. Gaps, overlap, articulation, pedal, and release determine perceived rhythm even when note starts are unchanged.
- Use velocity as an accent/dynamic signal, not as indiscriminate random noise. Repeated notes may vary when a performer would redirect the gesture.
- Check whether pedal turns written separations into continuous sound; audible rhythm follows the rendered envelope, not only MIDI note boundaries.

The editor's `quantize_notes` command is a reversible timing tool, not a style
rule. Use a deliberately chosen grid and strength when the brief calls for a
stable pulse; use partial strength or leave selected onsets untouched when
human timing, rubato, or metric ambiguity is part of the expression. It never
changes note duration, velocity, or pedal controls.

## Audit questions

- What pulse or grouping can be heard, and where is it intentionally weakened?
- Which onset/duration pattern identifies the motif or texture?
- Where does rhythmic tension accumulate and release?
- Does variation preserve enough identity to be perceived as development?
- Is silence carrying phrase structure, or is it an accidental hole?

These are advisories. Irregular, arrhythmic, highly quantized, or extremely sparse writing is valid when it realizes the brief.

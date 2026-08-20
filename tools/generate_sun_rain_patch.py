#!/usr/bin/env python3
import json
import sys


PPQ = 960
BAR = PPQ * 4


def midi(note):
    names = {
        "C": 0,
        "C#": 1,
        "Db": 1,
        "D": 2,
        "D#": 3,
        "Eb": 3,
        "E": 4,
        "F": 5,
        "F#": 6,
        "Gb": 6,
        "G": 7,
        "G#": 8,
        "Ab": 8,
        "A": 9,
        "A#": 10,
        "Bb": 10,
        "B": 11,
    }
    if len(note) == 2:
        name, octave = note[0], int(note[1])
    else:
        name, octave = note[:2], int(note[2])
    return (octave + 1) * 12 + names[name]


CHORDS = {
    "Am": [45, 52, 57, 60, 64],
    "Am/C": [48, 52, 57, 60, 64],
    "Am/E": [40, 45, 52, 57, 60],
    "Am/G": [43, 45, 52, 57, 60],
    "Amadd9": [45, 52, 57, 59, 60, 64],
    "Fmaj7": [41, 48, 53, 57, 64],
    "C": [36, 43, 48, 52, 55],
    "C/E": [40, 43, 48, 52, 55],
    "G": [43, 50, 55, 59, 62],
    "G/B": [47, 50, 55, 59, 62],
    "G7": [43, 50, 55, 59, 65],
    "Dm7": [38, 45, 50, 53, 60],
    "E7": [40, 47, 52, 56, 62],
    "E7b9": [40, 47, 52, 56, 62, 65],
    "Em/G": [43, 47, 52, 55, 59],
    "F#m7b5": [42, 48, 52, 57, 60],
    "Bbmaj7": [46, 53, 58, 62, 69],
    "Fm": [41, 48, 53, 56, 60],
    "E/G#": [44, 47, 52, 56, 59],
}


PROGRESSION = [
    "Am", "Fmaj7", "C/E", "G", "Am", "Fmaj7", "Dm7", "E7",
    "C", "G/B", "Am", "Em/G", "Fmaj7", "C/E", "Dm7", "E7",
    "Am/G", "F#m7b5", "Fmaj7", "Dm7", "Bbmaj7", "Am/E", "Fm", "E7b9",
    "Am", "Fmaj7", "C/E", "G", "Dm7", "Am/E", "Fmaj7", "G",
    "C", "G/B", "Am", "E/G#", "Fmaj7", "C/E", "Dm7", "G7",
    "Am", "Fmaj7", "C/E", "G", "Dm7", "Am/E", "E7b9", "Amadd9",
]


MELODY = [
    [("E5", .5), ("G5", .5), ("A5", 1), ("C6", .5), ("B5", .5), ("A5", 1)],
    [("A5", .5), ("G5", .5), ("F5", 1), ("E5", .5), ("C5", .5), ("E5", 1)],
    [("G5", .5), ("E5", .5), ("G5", 1), ("A5", .5), ("G5", .5), ("E5", 1)],
    [("D5", .5), ("G5", .5), ("A5", .5), ("B5", .5), ("D6", 1), ("B5", 1)],
    [("E5", .5), ("G5", .5), ("A5", .5), ("C6", .5), ("E6", 1), ("D6", .5), ("C6", .5)],
    [("C6", .5), ("A5", .5), ("F5", 1), ("A5", .5), ("G5", .5), ("F5", .5), ("E5", .5)],
    [("F5", .5), ("A5", .5), ("D6", 1), ("C6", .5), ("A5", .5), ("F5", 1)],
    [("G#5", .5), ("B5", .5), ("E6", 1), ("D6", .5), ("B5", .5), ("G#5", .5), ("E5", .5)],
    [("G5", .5), ("C6", .5), ("E6", 1), ("D6", .5), ("C6", .5), ("G5", 1)],
    [("D6", .5), ("B5", .5), ("G5", 1), ("A5", .5), ("G5", .5), ("D5", 1)],
    [("E5", .5), ("G5", .5), ("A5", .5), ("C6", .5), ("E6", .5), ("D6", .5), ("C6", 1)],
    [("B5", .5), ("G5", .5), ("E5", 1), ("G5", .5), ("A5", .5), ("B5", 1)],
    [("A5", .5), ("C6", .5), ("F6", 1), ("E6", .5), ("C6", .5), ("A5", 1)],
    [("G5", .5), ("E5", .5), ("G5", .5), ("C6", .5), ("B5", 1), ("G5", 1)],
    [("F5", .5), ("A5", .5), ("D6", .5), ("F6", .5), ("E6", 1), ("D6", .5), ("C6", .5)],
    [("B5", .5), ("G#5", .5), ("B5", .5), ("D6", .5), ("E6", 1), ("G#5", 1)],
    [("A4", 1), ("C5", 1), ("E5", 1), ("D5", 1)],
    [("C5", 1), ("F#5", .5), ("A5", .5), ("G5", 1), ("E5", 1)],
    [("F5", 1), ("A5", 1), ("C6", .5), ("A5", .5), ("E5", 1)],
    [("D5", 1), ("F5", 1), ("A5", 1), ("C6", 1)],
    [("D5", 1), ("F5", 1), ("Bb5", 1), ("A5", 1)],
    [("E5", .5), ("A5", .5), ("C6", 1), ("B5", 1), ("A5", 1)],
    [("Ab5", 1), ("G5", 1), ("F5", 1), ("C5", 1)],
    [("F5", .5), ("E5", .5), ("G#5", 1), ("B5", 1), (None, .5), ("E5", .5)],
    [("E5", .5), ("G5", .5), ("A5", .5), ("C6", .5), ("E6", .5), ("D6", .5), ("C6", 1)],
    [("A5", .5), ("C6", .5), ("F6", .5), ("E6", .5), ("C6", .5), ("A5", .5), ("G5", 1)],
    [("G5", .5), ("C6", .5), ("E6", .5), ("G6", .5), ("E6", .5), ("D6", .5), ("C6", 1)],
    [("D5", .5), ("G5", .5), ("B5", .5), ("D6", .5), ("G6", 1), ("F6", .5), ("D6", .5)],
    [("F5", .5), ("A5", .5), ("D6", .5), ("F6", .5), ("A6", 1), ("F6", .5), ("E6", .5)],
    [("E5", .5), ("A5", .5), ("C6", .5), ("E6", .5), ("A6", 1), ("G6", .5), ("E6", .5)],
    [("A5", .5), ("C6", .5), ("F6", .5), ("A6", .5), ("C7", 1), ("A6", .5), ("F6", .5)],
    [("G5", .5), ("B5", .5), ("D6", .5), ("G6", .5), ("B6", .5), ("A6", .5), ("G6", 1)],
    [("G5", .5), ("C6", .5), ("E6", .5), ("G6", .5), ("C7", 1), ("B6", .5), ("G6", .5)],
    [("D6", .5), ("G6", .5), ("B6", 1), ("A6", .5), ("G6", .5), ("D6", 1)],
    [("E6", .5), ("A6", .5), ("C7", 1), ("B6", .5), ("A6", .5), ("E6", 1)],
    [("E6", .5), ("G#6", .5), ("B6", .5), ("E7", .5), ("D7", 1), ("B6", 1)],
    [("A5", .5), ("C6", .5), ("F6", .5), ("A6", .5), ("C7", 1), ("A6", .5), ("G6", .5)],
    [("G5", .5), ("C6", .5), ("E6", .5), ("G6", .5), ("B6", .5), ("G6", .5), ("E6", 1)],
    [("A5", .5), ("D6", .5), ("F6", .5), ("A6", .5), ("D7", 1), ("C7", .5), ("A6", .5)],
    [("B5", .5), ("D6", .5), ("G6", .5), ("B6", .5), ("F7", 1), ("D7", .5), ("B6", .5)],
    [("E5", .5), ("G5", .5), ("A5", 1), ("C6", .5), ("B5", .5), ("A5", 1)],
    [("A5", .5), ("G5", .5), ("F5", 1), ("E5", .5), ("C5", .5), ("E5", 1)],
    [("G5", .5), ("C6", .5), ("E6", 1), ("D6", .5), ("C6", .5), ("G5", 1)],
    [("D5", .5), ("G5", .5), ("B5", 1), ("A5", .5), ("G5", .5), ("D5", 1)],
    [("F5", 1), ("A5", 1), ("D6", 1), ("C6", 1)],
    [("E5", 1), ("A5", 1), ("C6", 1), ("B5", 1)],
    [("F5", .5), ("E5", .5), ("G#5", 1), ("B5", 1), ("D6", 1)],
    [("A5", 1), ("E6", 1), ("B6", 2)],
]


MELODY_BASE = [
    66, 67, 68, 70, 72, 70, 73, 75,
    74, 73, 76, 75, 78, 76, 80, 76,
    55, 57, 58, 59, 60, 61, 56, 62,
    70, 72, 74, 76, 78, 80, 83, 85,
    87, 88, 90, 92, 94, 92, 96, 98,
    74, 72, 70, 68, 64, 61, 59, 55,
]


def add_note(ops, counters, bar, beat, duration, pitch, velocity, role, humanize=True):
    counters[role] = counters.get(role, 0) + 1
    jitter = 0
    if humanize:
        jitter_pattern = [7, -5, 11, -8, 4, -3, 9, -6]
        jitter = jitter_pattern[(counters[role] - 1) % len(jitter_pattern)]
    start = bar * BAR + round(beat * PPQ) + jitter
    start = max(bar * BAR, start)
    duration_tick = max(120, round(duration * PPQ))
    velocity = max(1, min(127, round(velocity)))
    ops.append({
        "op": "add_note",
        "track_id": "piano",
        "clip_id": "sun-rain-main",
        "note": {
            "id": f"b{bar + 1:02d}-{role}-{counters[role]:03d}",
            "start_tick": start,
            "duration_tick": duration_tick,
            "pitch": pitch,
            "velocity": velocity,
        },
    })


def add_control(ops, control_id, tick, controller, value):
    ops.append({
        "op": "add_control",
        "track_id": "piano",
        "clip_id": "sun-rain-main",
        "control": {
            "id": control_id,
            "tick": tick,
            "controller": controller,
            "value": value,
        },
    })


def accompaniment_velocity(bar):
    if bar < 8:
        return 40 + bar // 2
    if bar < 16:
        return 45 + (bar - 8) // 2
    if bar < 24:
        return 31 + (bar - 16)
    if bar < 32:
        return 42 + (bar - 24) * 2
    if bar < 40:
        return 48 + (bar - 32)
    return max(30, 43 - (bar - 40) * 2)


def build_patch():
    ops = [
        {"op": "rename_track", "track_id": "piano", "name": "晴雨之间 — 独奏钢琴"},
        {
            "op": "add_clip",
            "track_id": "piano",
            "clip_id": "sun-rain-main",
            "start_tick": 0,
            "length_tick": 48 * BAR,
        },
        {
            "op": "set_track_mixer",
            "track_id": "piano",
            "gain_db": -4.0,
            "pan": 0.0,
            "mute": False,
            "solo": False,
        },
        {"op": "set_time_signature", "numerator": 4, "denominator": 4},
    ]

    tempos = [
        (0, 108.0), (8, 114.0), (16, 98.0), (20, 92.0),
        (24, 108.0), (28, 116.0), (32, 122.0), (36, 126.0),
        (40, 108.0), (44, 96.0), (46, 84.0), (47, 72.0),
    ]
    for bar, bpm in tempos:
        ops.append({"op": "set_tempo", "tick": bar * BAR, "bpm": bpm})

    counters = {}
    add_control(ops, "soft-opening", 0, 67, 20)
    add_control(ops, "soft-valley", 16 * BAR, 67, 82)
    add_control(ops, "soft-build-release", 24 * BAR, 67, 0)
    add_control(ops, "soft-coda", 40 * BAR, 67, 52)
    add_control(ops, "soft-final-release", 48 * BAR - 100, 67, 0)

    for bar, chord_name in enumerate(PROGRESSION):
        chord = CHORDS[chord_name]
        acc_base = accompaniment_velocity(bar)

        if 16 <= bar < 24 or bar >= 44:
            pattern = [0, 2, 1, min(3, len(chord) - 1)]
            for step, chord_index in enumerate(pattern):
                add_note(
                    ops, counters, bar, step, .82,
                    chord[chord_index], acc_base + (5 if step == 0 else 0), "a",
                )
        elif 32 <= bar < 40:
            for beat in (0, 2):
                add_note(ops, counters, bar, beat, .86, chord[0], acc_base + 13, "bass")
            upper = list(range(1, min(5, len(chord))))
            pattern = (upper + upper[-2:0:-1]) or [1]
            for step in range(16):
                chord_index = pattern[step % len(pattern)]
                accent = 5 if step % 4 == 0 else 0
                add_note(
                    ops, counters, bar, step * .25, .21,
                    chord[chord_index], acc_base - 3 + accent, "a",
                )
        else:
            pattern = [0, 1, 2, 3, min(4, len(chord) - 1), 3, 2, 1]
            for step, chord_index in enumerate(pattern):
                accent = 7 if step in (0, 4) else 0
                add_note(
                    ops, counters, bar, step * .5, .43,
                    chord[chord_index], acc_base + accent, "a",
                )
            if bar in (24, 28):
                low = chord[0] - 12
                if low >= 28:
                    add_note(ops, counters, bar, 0, .88, low, acc_base + 12, "bass")

        pedal = 68
        if 8 <= bar < 16:
            pedal = 82
        elif 16 <= bar < 24:
            pedal = 56
        elif 24 <= bar < 32:
            pedal = 78
        elif 32 <= bar < 40:
            pedal = 92
        elif bar >= 44:
            pedal = 64
        add_control(ops, f"pedal-{bar + 1:02d}-down", bar * BAR + 24, 64, pedal)
        add_control(ops, f"pedal-{bar + 1:02d}-up", (bar + 1) * BAR - 90, 64, 0)

        beat = 0.0
        for note_name, span in MELODY[bar]:
            if note_name is not None:
                pitch = midi(note_name)
                accent = 5 if abs(beat - round(beat)) < 0.01 else 0
                contour = max(0, (pitch - 72) // 5)
                gate = .93 if span >= 1 else .86
                velocity = MELODY_BASE[bar] + accent + contour
                add_note(
                    ops, counters, bar, beat, span * gate,
                    pitch, velocity, "m",
                )
                if 32 <= bar < 40 and (span >= 1 or beat in (0, 2)):
                    add_note(
                        ops, counters, bar, beat, span * gate,
                        pitch - 12, velocity - 15, "oct",
                    )
            beat += span
        if abs(beat - 4.0) > 0.001:
            raise ValueError(f"melody bar {bar + 1} totals {beat} beats")

    # A final A-minor-add-nine sonority keeps the ending luminous but unresolved.
    for index, pitch in enumerate([45, 52, 57, 60, 64, 71]):
        add_note(
            ops, counters, 47, 2, 1.82,
            pitch, 43 + index * 3, "final", humanize=False,
        )

    return {
        "base_revision": 0,
        "description": (
            "《晴雨之间》：48 小节独奏钢琴曲。明快流动的分解和弦承载小调旋律，"
            "经低谷、重建与高音区高潮后，停在带九度音的 A 小调余韵中。"
        ),
        "operations": ops,
    }


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: generate_sun_rain_patch.py OUTPUT.json")
    patch = build_patch()
    with open(sys.argv[1], "w", encoding="utf-8") as output:
        json.dump(patch, output, ensure_ascii=False, indent=2)
        output.write("\n")
    print(json.dumps({"operations": len(patch["operations"])}, ensure_ascii=False))


if __name__ == "__main__":
    main()

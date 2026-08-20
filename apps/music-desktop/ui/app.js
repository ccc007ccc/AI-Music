const invoke = window.__TAURI__?.core?.invoke;
const editor = document.querySelector(".editor");
const canvas = document.getElementById("piano-roll");
const context = canvas.getContext("2d");
const ruler = document.getElementById("ruler");
const trackList = document.getElementById("track-list");
const emptyState = document.getElementById("empty-state");
const statusText = document.getElementById("status-text");
const revisionText = document.getElementById("revision");
const tempoInput = document.getElementById("tempo");
const meterNumerator = document.getElementById("meter-numerator");
const meterDenominator = document.getElementById("meter-denominator");
const gridInput = document.getElementById("grid");
const quantizeStrength = document.getElementById("quantize-strength");
const projectNameText = document.getElementById("project-name");
const projectModal = document.getElementById("project-modal");
const projectModalTitle = document.getElementById("project-modal-title");
const projectNameInput = document.getElementById("project-name-input");
const projectLocation = document.getElementById("project-location");
const confirmProjectModal = document.getElementById("confirm-project-modal");
const aiInstruction = document.getElementById("ai-instruction");
const runAutopilotButton = document.getElementById("run-autopilot");
const aiResult = document.getElementById("ai-result");

let project = null;
let workspace = null;
let activeTrack = null;
let selectedNoteId = null;
let drag = null;
let previewNote = null;
let playbackAnimation = null;
let playbackTick = null;
let lastPlaybackPoll = 0;
let didSetInitialVerticalScroll = false;
let instrumentCatalog = [];
let pianoAsset = null;
let projectModalMode = "new";
let selectedProjectParent = null;

const pixelsPerBeat = 84;
const rowHeight = 12;
const highestPitch = 108;
const lowestPitch = 21;
const visibleRows = highestPitch - lowestPitch + 1;

function setStatus(value) {
  statusText.textContent = value;
}

async function call(name, args) {
  if (!invoke) {
    setStatus("请在 Tauri 应用中运行");
    return null;
  }
  try {
    return await invoke(name, args);
  } catch (error) {
    setStatus(String(error));
    return null;
  }
}

async function action(name, args) {
  if (!invoke) {
    setStatus("请在 Tauri 应用中运行");
    return false;
  }
  try {
    await invoke(name, args);
    return true;
  } catch (error) {
    setStatus(String(error));
    return false;
  }
}

function ppq() {
  return project?.ppq || 960;
}

function gridTick() {
  const values = {
    quarter: ppq(),
    eighth: ppq() / 2,
    sixteenth: ppq() / 4,
    "thirty-second": ppq() / 8,
    "eighth-triplet": ppq() / 3,
    "sixteenth-triplet": ppq() / 6,
  };
  return Math.max(1, Math.round(values[gridInput?.value] || ppq() / 4));
}

function beatsPerBar() {
  const signature = project?.time_signature;
  if (!signature) return 4;
  return signature.numerator * (4 / signature.denominator);
}

function barTick() {
  return Math.max(1, Math.round(ppq() * beatsPerBar()));
}

function currentClip() {
  const track =
    project?.tracks.find((item) => item.id === activeTrack) || project?.tracks[0];
  if (!track || track.source.kind !== "midi") return null;
  const clips = track.source.clips || [];
  // Prefer material-bearing clips so an empty setup clip cannot hide the song.
  const clip = clips.find((item) => (item.notes?.length || 0) > 0 || (item.controls?.length || 0) > 0) || clips[0];
  return clip ? { track, clip } : null;
}

function projectEndTick() {
  if (!project) return ppq() * 16;
  let end = ppq() * 16;
  for (const track of project.tracks) {
    for (const clip of track.source.clips || []) {
      let clipEnd = clip.start_tick + clip.length_tick;
      for (const note of clip.notes || []) {
        clipEnd = Math.max(
          clipEnd,
          clip.start_tick + note.start_tick + note.duration_tick,
        );
      }
      end = Math.max(end, clipEnd);
    }
  }
  return Math.ceil(end / barTick()) * barTick();
}

function tickToX(tick) {
  return (tick / ppq()) * pixelsPerBeat;
}

function xToTick(x) {
  return (x / pixelsPerBeat) * ppq();
}

function snapTick(tick) {
  return Math.round(tick / gridTick()) * gridTick();
}

function pitchAtY(y) {
  return Math.max(
    lowestPitch,
    Math.min(highestPitch, highestPitch - Math.floor(y / rowHeight)),
  );
}

function noteRect(note, clip) {
  const shown = previewNote?.id === note.id ? previewNote : note;
  const absoluteStart = clip.start_tick + shown.start_tick;
  return {
    x: tickToX(absoluteStart),
    y: (highestPitch - shown.pitch) * rowHeight + 1,
    width: Math.max(4, tickToX(shown.duration_tick) - 2),
    height: rowHeight - 2,
    note: shown,
  };
}

function hitTest(x, y) {
  const current = currentClip();
  if (!current) return null;
  const notes = current.clip.notes || [];
  for (let index = notes.length - 1; index >= 0; index -= 1) {
    const note = notes[index];
    if (note.pitch < lowestPitch || note.pitch > highestPitch) continue;
    const rect = noteRect(note, current.clip);
    if (
      x >= rect.x &&
      x <= rect.x + rect.width &&
      y >= rect.y &&
      y <= rect.y + rect.height
    ) {
      const handleWidth = Math.min(9, Math.max(5, rect.width * 0.28));
      return {
        note,
        mode: x >= rect.x + rect.width - handleWidth ? "resize" : "move",
      };
    }
  }
  return null;
}

function renderRuler(width) {
  ruler.replaceChildren();
  ruler.style.width = `${width}px`;
  const lastTick = projectEndTick();
  for (let tick = 0, bar = 1; tick <= lastTick; tick += barTick(), bar += 1) {
    const marker = document.createElement("div");
    marker.className = "bar-marker";
    marker.style.left = `${tickToX(tick)}px`;
    marker.textContent = String(bar);
    ruler.append(marker);
  }
}

function positionEmptyState() {
  emptyState.style.left = `${editor.scrollLeft + editor.clientWidth * 0.5}px`;
  emptyState.style.top = `${editor.scrollTop + editor.clientHeight * 0.52}px`;
}

function resizeCanvas() {
  const timelineWidth = Math.max(
    editor.clientWidth,
    Math.ceil(tickToX(projectEndTick()) + pixelsPerBeat),
  );
  const timelineHeight = Math.max(
    editor.clientHeight - ruler.offsetHeight,
    visibleRows * rowHeight,
  );
  canvas.style.width = `${timelineWidth}px`;
  canvas.style.height = `${timelineHeight}px`;
  renderRuler(timelineWidth);

  const ratio = window.devicePixelRatio || 1;
  canvas.width = Math.max(1, Math.floor(timelineWidth * ratio));
  canvas.height = Math.max(1, Math.floor(timelineHeight * ratio));
  context.setTransform(ratio, 0, 0, ratio, 0, 0);
  draw();

  if (project && !didSetInitialVerticalScroll) {
    const middleCY = (highestPitch - 60) * rowHeight;
    editor.scrollTop = Math.max(
      0,
      ruler.offsetHeight + middleCY - editor.clientHeight * 0.5,
    );
    didSetInitialVerticalScroll = true;
  }
  positionEmptyState();
}

function draw() {
  const rect = canvas.getBoundingClientRect();
  const width = rect.width;
  const height = rect.height;
  context.clearRect(0, 0, width, height);
  context.fillStyle = "#101217";
  context.fillRect(0, 0, width, height);

  for (let pitch = lowestPitch; pitch <= highestPitch; pitch += 1) {
    const y = (highestPitch - pitch) * rowHeight;
    const isBlackKey = [1, 3, 6, 8, 10].includes(pitch % 12);
    context.fillStyle = isBlackKey ? "#0d0f13" : "#12151b";
    context.fillRect(0, y, width, rowHeight);
    context.strokeStyle = pitch % 12 === 0 ? "#2b303a" : "#20242c";
    context.beginPath();
    context.moveTo(0, y + rowHeight);
    context.lineTo(width, y + rowHeight);
    context.stroke();
  }

  const subdivision = gridTick();
  const lastTick = Math.ceil(xToTick(width) / subdivision) * subdivision;
  for (let tick = 0; tick <= lastTick; tick += subdivision) {
    const x = tickToX(tick);
    if (tick % barTick() === 0) {
      context.strokeStyle = "#3b414e";
    } else if (tick % ppq() === 0) {
      context.strokeStyle = "#292e38";
    } else {
      context.strokeStyle = "#1b1f27";
    }
    context.beginPath();
    context.moveTo(x, 0);
    context.lineTo(x, height);
    context.stroke();
  }

  const current = currentClip();
  const notes = current?.clip?.notes || [];
  emptyState.textContent = current ? "双击网格添加音符" : "点击 + 新建钢琴轨";
  emptyState.classList.toggle("hidden", notes.length > 0);
  for (const note of notes) {
    const shown = previewNote?.id === note.id ? previewNote : note;
    if (shown.pitch < lowestPitch || shown.pitch > highestPitch) continue;
    const noteBox = noteRect(note, current.clip);
    const selected = note.id === selectedNoteId;
    const alpha = 0.52 + shown.velocity / 300;
    context.fillStyle = selected
      ? "#c1f3cf"
      : `rgba(168, 230, 187, ${alpha})`;
    context.fillRect(noteBox.x, noteBox.y, noteBox.width, noteBox.height);
    if (selected) {
      context.strokeStyle = "#f4fff7";
      context.lineWidth = 1;
      context.strokeRect(
        noteBox.x + 0.5,
        noteBox.y + 0.5,
        Math.max(0, noteBox.width - 1),
        Math.max(0, noteBox.height - 1),
      );
      context.fillStyle = "rgba(13, 19, 17, 0.5)";
      context.fillRect(noteBox.x + noteBox.width - 5, noteBox.y, 5, noteBox.height);
    }
  }

  if (playbackTick !== null) {
    const x = tickToX(playbackTick);
    context.strokeStyle = "#f4c66b";
    context.lineWidth = 1.5;
    context.beginPath();
    context.moveTo(x, 0);
    context.lineTo(x, height);
    context.stroke();
  }
}

function formatGain(value) {
  const rounded = Math.round(value * 10) / 10;
  const sign = rounded > 0 ? "+" : "";
  return `${sign}${rounded} dB`;
}

function formatPan(value) {
  if (Math.abs(value) < 0.005) return "C";
  const side = value < 0 ? "L" : "R";
  return `${side}${Math.round(Math.abs(value) * 100)}`;
}

function ensureSelectValue(select, value) {
  const stringValue = String(value);
  if (![...select.options].some((option) => option.value === stringValue)) {
    const option = document.createElement("option");
    option.value = stringValue;
    option.textContent = stringValue;
    select.append(option);
  }
  select.value = stringValue;
}

function renderProject(snapshot) {
  workspace = snapshot?.project ? snapshot : null;
  project = snapshot?.project || snapshot;
  if (workspace) {
    projectNameText.textContent = `${workspace.name || "未命名工程"}${workspace.dirty ? " *" : ""}`;
    projectNameText.title = workspace.path || workspace.name || "未命名工程";
  }
  if (!project.tracks.some((track) => track.id === activeTrack)) {
    activeTrack = project.tracks[0]?.id || null;
  }

  const selectedStillExists = currentClip()?.clip.notes.some(
    (note) => note.id === selectedNoteId,
  );
  if (!selectedStillExists) selectedNoteId = null;

  trackList.replaceChildren();
  for (const track of project.tracks) {
    const item = document.createElement("div");
    item.className = `track${track.id === activeTrack ? " active" : ""}${
      track.mixer.mute ? " muted" : ""
    }`;
    item.innerHTML = `
      <div class="track-heading">
        <div class="track-name" title="双击重命名"></div>
        <div class="track-actions">
          <button class="track-action mute" title="静音">M</button>
          <button class="track-action solo" title="独奏">S</button>
          <button class="track-action remove" title="删除音轨">×</button>
        </div>
      </div>
      <select class="track-instrument" aria-label="乐器" title="乐器"></select>
      <div class="track-mixer">
        <label class="mixer-control" title="音量">
          <span>音量</span>
          <input class="gain" type="range" min="-96" max="24" step="0.5" />
          <output class="gain-value"></output>
        </label>
        <label class="mixer-control" title="声像">
          <span>声像</span>
          <input class="pan" type="range" min="-1" max="1" step="0.01" />
          <output class="pan-value"></output>
        </label>
      </div>`;
    item.querySelector(".track-name").textContent = track.name;
    const instrumentSelect = item.querySelector(".track-instrument");
    if (track.source.kind === "midi") {
      for (const instrument of instrumentCatalog) {
        const option = document.createElement("option");
        option.value = instrument.id;
        option.textContent = instrument.name;
        instrumentSelect.append(option);
      }
      if (!instrumentCatalog.some(({ id }) => id === track.source.instrument)) {
        const option = document.createElement("option");
        option.value = track.source.instrument;
        option.textContent = track.source.instrument;
        instrumentSelect.append(option);
      }
      instrumentSelect.value = track.source.instrument;
    } else {
      const option = document.createElement("option");
      option.textContent = "Audio";
      instrumentSelect.append(option);
      instrumentSelect.disabled = true;
    }

    const gainInput = item.querySelector(".gain");
    const panInput = item.querySelector(".pan");
    const gainValue = item.querySelector(".gain-value");
    const panValue = item.querySelector(".pan-value");
    gainInput.value = String(track.mixer.gain_db);
    panInput.value = String(track.mixer.pan);
    gainValue.textContent = formatGain(track.mixer.gain_db);
    panValue.textContent = formatPan(track.mixer.pan);
    const muteButton = item.querySelector(".mute");
    const soloButton = item.querySelector(".solo");
    muteButton.classList.toggle("enabled", track.mixer.mute);
    soloButton.classList.toggle("enabled", track.mixer.solo);

    item.addEventListener("click", () => {
      if (activeTrack !== track.id) {
        activeTrack = track.id;
        selectedNoteId = null;
        previewNote = null;
      }
      renderProject(workspace || project);
    });
    item.querySelector(".track-name").addEventListener("dblclick", (event) => {
      event.stopPropagation();
      beginTrackRename(item.querySelector(".track-name"), track);
    });
    for (const control of [instrumentSelect, gainInput, panInput]) {
      control.addEventListener("click", (event) => event.stopPropagation());
    }
    instrumentSelect.addEventListener("change", async (event) => {
      event.stopPropagation();
      if (
        track.source.kind === "midi" &&
        instrumentSelect.value !== track.source.instrument &&
        (await action("apply_command", {
          command: {
            op: "set_track_instrument",
            track_id: track.id,
            instrument: instrumentSelect.value,
          },
        }))
      ) {
        setStatus("已切换乐器");
        await refresh();
      }
    });
    gainInput.addEventListener("input", () => {
      gainValue.textContent = formatGain(Number(gainInput.value));
    });
    gainInput.addEventListener("change", async (event) => {
      event.stopPropagation();
      if (await setTrackMixer(track, { gain_db: Number(gainInput.value) })) {
        setStatus("已修改音量");
        await refresh();
      }
    });
    panInput.addEventListener("input", () => {
      panValue.textContent = formatPan(Number(panInput.value));
    });
    panInput.addEventListener("change", async (event) => {
      event.stopPropagation();
      if (await setTrackMixer(track, { pan: Number(panInput.value) })) {
        setStatus("已修改声像");
        await refresh();
      }
    });
    muteButton.addEventListener("click", async (event) => {
      event.stopPropagation();
      const enabled = !track.mixer.mute;
      if (await setTrackMixer(track, { mute: enabled, solo: enabled ? false : track.mixer.solo })) {
        setStatus(enabled ? "音轨已静音" : "音轨已取消静音");
        await refresh();
      }
    });
    soloButton.addEventListener("click", async (event) => {
      event.stopPropagation();
      const enabled = !track.mixer.solo;
      if (await setTrackMixer(track, { solo: enabled, mute: enabled ? false : track.mixer.mute })) {
        setStatus(enabled ? "音轨已独奏" : "音轨已取消独奏");
        await refresh();
      }
    });
    item.querySelector(".remove").addEventListener("click", async (event) => {
      event.stopPropagation();
      if (!window.confirm(`删除“${track.name}”及其全部音符？`)) return;
      if (
        await action("apply_command", {
          command: { op: "remove_track", track_id: track.id },
        })
      ) {
        if (activeTrack === track.id) activeTrack = null;
        selectedNoteId = null;
        previewNote = null;
        setStatus("已删除音轨");
        await refresh();
      }
    });
    trackList.append(item);
  }
  tempoInput.value = Math.round(project.tempo_map.points[0]?.bpm || 120);
  if (project.time_signature) {
    ensureSelectValue(meterNumerator, project.time_signature.numerator);
    ensureSelectValue(meterDenominator, project.time_signature.denominator);
  }
  revisionText.textContent = `${workspace?.dirty ? "未保存 · " : ""}版本 ${project.revision || 0}`;
  resizeCanvas();
}

function beginTrackRename(nameElement, track) {
  if (nameElement.querySelector("input")) return;
  const input = document.createElement("input");
  input.className = "track-name-input";
  input.value = track.name;
  nameElement.replaceChildren(input);
  input.focus();
  input.select();
  let finished = false;
  const finish = async (commit) => {
    if (finished) return;
    finished = true;
    const name = input.value.trim();
    if (commit && name && name !== track.name) {
      if (await action("apply_command", {
        command: { op: "rename_track", track_id: track.id, name },
      })) {
        setStatus("已重命名音轨");
        await refresh();
        return;
      }
    }
    nameElement.textContent = track.name;
  };
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter") finish(true);
    if (event.key === "Escape") finish(false);
  });
  input.addEventListener("blur", () => finish(true));
}

async function setTrackMixer(track, changes) {
  return action("apply_command", {
    command: {
      op: "set_track_mixer",
      track_id: track.id,
      gain_db: track.mixer.gain_db,
      pan: track.mixer.pan,
      mute: track.mixer.mute,
      solo: track.mixer.solo,
      ...changes,
    },
  });
}

function uniqueId(prefix) {
  const suffix = window.crypto?.randomUUID
    ? window.crypto.randomUUID()
    : `${Date.now()}-${Math.floor(Math.random() * 1_000_000)}`;
  return `${prefix}-${suffix}`;
}

async function refresh() {
  const availableInstruments = await call("instrument_catalog");
  if (Array.isArray(availableInstruments)) {
    instrumentCatalog = availableInstruments;
  }
  pianoAsset = await call("piano_asset_snapshot");
  const assetButton = document.getElementById("choose-piano-asset");
  assetButton.title = pianoAsset?.name
    ? `${pianoAsset.name}\n${pianoAsset.location || ""}`
    : "选择授权的 SF2 或 SFZ 钢琴资源包";
  assetButton.classList.toggle("active-source", Boolean(pianoAsset?.asset_id));
  const snapshot = await call("workspace_snapshot");
  if (snapshot) renderProject(snapshot);
}

function pointerPosition(event) {
  const rect = canvas.getBoundingClientRect();
  return { x: event.clientX - rect.left, y: event.clientY - rect.top };
}

canvas.addEventListener("pointerdown", (event) => {
  if (event.button !== 0) return;
  const current = currentClip();
  if (!current) return;
  const position = pointerPosition(event);
  const hit = hitTest(position.x, position.y);
  if (!hit) {
    selectedNoteId = null;
    previewNote = null;
    draw();
    return;
  }

  selectedNoteId = hit.note.id;
  previewNote = { ...hit.note };
  drag = {
    pointerId: event.pointerId,
    mode: hit.mode,
    note: { ...hit.note },
    startX: position.x,
    startY: position.y,
    moved: false,
  };
  canvas.setPointerCapture(event.pointerId);
  canvas.style.cursor = hit.mode === "resize" ? "ew-resize" : "grabbing";
  event.preventDefault();
  draw();
});

canvas.addEventListener("pointermove", (event) => {
  const position = pointerPosition(event);
  if (!drag || event.pointerId !== drag.pointerId) {
    const hit = hitTest(position.x, position.y);
    canvas.style.cursor = hit
      ? hit.mode === "resize"
        ? "ew-resize"
        : "grab"
      : "crosshair";
    return;
  }

  const deltaTick = xToTick(position.x - drag.startX);
  if (drag.mode === "move") {
    const startTick = Math.max(0, snapTick(drag.note.start_tick + deltaTick));
    const pitchDelta = Math.round((drag.startY - position.y) / rowHeight);
    const pitch = Math.max(
      lowestPitch,
      Math.min(highestPitch, drag.note.pitch + pitchDelta),
    );
    previewNote = { ...drag.note, start_tick: startTick, pitch };
    drag.moved = startTick !== drag.note.start_tick || pitch !== drag.note.pitch;
  } else {
    const durationTick = Math.max(
      gridTick(),
      snapTick(drag.note.duration_tick + deltaTick),
    );
    previewNote = { ...drag.note, duration_tick: durationTick };
    drag.moved = durationTick !== drag.note.duration_tick;
  }
  draw();
});

async function finishDrag(event, cancelled = false) {
  if (!drag || event.pointerId !== drag.pointerId) return;
  const currentDrag = drag;
  const finalNote = previewNote;
  drag = null;
  previewNote = null;
  canvas.style.cursor = "crosshair";
  if (canvas.hasPointerCapture(event.pointerId)) {
    canvas.releasePointerCapture(event.pointerId);
  }
  if (cancelled || !currentDrag.moved || !finalNote) {
    draw();
    return;
  }

  const current = currentClip();
  if (!current) return;
  const command =
    currentDrag.mode === "move"
      ? {
          op: "move_note",
          track_id: current.track.id,
          clip_id: current.clip.id,
          note_id: currentDrag.note.id,
          start_tick: finalNote.start_tick,
          pitch: finalNote.pitch,
        }
      : {
          op: "resize_note",
          track_id: current.track.id,
          clip_id: current.clip.id,
          note_id: currentDrag.note.id,
          duration_tick: finalNote.duration_tick,
        };
  if (await action("apply_command", { command })) {
    setStatus(currentDrag.mode === "move" ? "已移动音符" : "已调整时值");
    await refresh();
  } else {
    draw();
  }
}

canvas.addEventListener("pointerup", (event) => finishDrag(event));
canvas.addEventListener("pointercancel", (event) => finishDrag(event, true));

canvas.addEventListener("dblclick", async (event) => {
  const current = currentClip();
  if (!current) return;
  const position = pointerPosition(event);
  if (hitTest(position.x, position.y)) return;
  const absoluteTick = Math.max(current.clip.start_tick, snapTick(xToTick(position.x)));
  const startTick = absoluteTick - current.clip.start_tick;
  const pitch = pitchAtY(position.y);
  const id = uniqueId("ui");
  const command = {
    op: "add_note",
    track_id: current.track.id,
    clip_id: current.clip.id,
    note: {
      id,
      start_tick: startTick,
      duration_tick: ppq(),
      pitch,
      velocity: 90,
    },
  };
  if (await action("apply_command", { command })) {
    selectedNoteId = id;
    setStatus("已添加音符");
    await refresh();
  }
});

function secondsToTick(seconds) {
  const points = [...(project?.tempo_map.points || [{ tick: 0, bpm: 120 }])].sort(
    (left, right) => left.tick - right.tick,
  );
  let elapsed = 0;
  let segmentTick = 0;
  let bpm = 120;
  for (const point of points) {
    if (point.tick <= 0) {
      bpm = point.bpm;
      continue;
    }
    const segmentSeconds = ((point.tick - segmentTick) * 60) / (bpm * ppq());
    if (elapsed + segmentSeconds >= seconds) break;
    elapsed += segmentSeconds;
    segmentTick = point.tick;
    bpm = point.bpm;
  }
  return segmentTick + ((seconds - elapsed) * bpm * ppq()) / 60;
}

async function animatePlayback(now) {
  if (now - lastPlaybackPoll >= 80) {
    lastPlaybackPoll = now;
    const snapshot = await call("playback_snapshot");
    if (snapshot?.error) {
      playbackAnimation = null;
      playbackTick = null;
      setStatus(`播放失败：${snapshot.error}`);
      draw();
      return;
    }
    if (snapshot?.rendering) {
      playbackTick = 0;
      if (statusText.textContent !== "准备播放") setStatus("准备播放");
      draw();
      playbackAnimation = window.requestAnimationFrame(animatePlayback);
      return;
    }
    if (snapshot?.playing && statusText.textContent === "准备播放") {
      setStatus("播放中");
    }
    if (!snapshot?.playing) {
      playbackAnimation = null;
      playbackTick = null;
      setStatus("播放完成");
      draw();
      return;
    }
    playbackTick = Math.min(
      projectEndTick(),
      secondsToTick(snapshot.elapsed_seconds),
    );
  }
  draw();

  const cursorX = tickToX(playbackTick);
  if (cursorX > editor.scrollLeft + editor.clientWidth - 80) {
    editor.scrollLeft = Math.max(0, cursorX - 80);
  }
  playbackAnimation = window.requestAnimationFrame(animatePlayback);
}

function startPlaybackCursor() {
  if (playbackAnimation !== null) {
    window.cancelAnimationFrame(playbackAnimation);
  }
  playbackTick = 0;
  lastPlaybackPoll = 0;
  playbackAnimation = window.requestAnimationFrame(animatePlayback);
}

function stopPlaybackCursor(reset = true) {
  if (playbackAnimation !== null) {
    window.cancelAnimationFrame(playbackAnimation);
    playbackAnimation = null;
  }
  if (reset) playbackTick = null;
  draw();
}

function openProjectModal(mode) {
  projectModalMode = mode;
  selectedProjectParent = null;
  projectModalTitle.textContent = mode === "new" ? "新建工程" : "保存工程";
  confirmProjectModal.textContent = mode === "new" ? "创建" : "保存";
  projectNameInput.value = mode === "save" && workspace?.name !== "未命名工程"
    ? workspace.name
    : "";
  projectLocation.textContent = "未选择";
  projectModal.classList.remove("hidden");
  window.setTimeout(() => projectNameInput.focus(), 0);
}

function closeProjectModal() {
  projectModal.classList.add("hidden");
  selectedProjectParent = null;
}

async function chooseProjectParent() {
  const location = await call("choose_project_location");
  if (!location) return false;
  selectedProjectParent = location;
  projectLocation.textContent = location;
  return true;
}

async function confirmProjectModalAction() {
  const name = projectNameInput.value.trim();
  if (!name) {
    setStatus("请输入工程名称");
    projectNameInput.focus();
    return;
  }
  if (!selectedProjectParent && !(await chooseProjectParent())) return;
  const command = projectModalMode === "new" ? "new_project" : "save_project";
  if (
    await action(command, {
      parent: selectedProjectParent,
      name,
    })
  ) {
    closeProjectModal();
    stopPlaybackCursor();
    selectedNoteId = null;
    setStatus(projectModalMode === "new" ? "已创建工程" : "已保存工程");
    await refresh();
  }
}

projectModal.addEventListener("click", (event) => {
  if (event.target === projectModal) closeProjectModal();
});
document.getElementById("choose-location").addEventListener("click", chooseProjectParent);
document.getElementById("cancel-project-modal").addEventListener("click", closeProjectModal);
confirmProjectModal.addEventListener("click", confirmProjectModalAction);

document.addEventListener("keydown", async (event) => {
  if (event.key === "Escape" && !projectModal.classList.contains("hidden")) {
    closeProjectModal();
    return;
  }
  const editingText = ["INPUT", "TEXTAREA"].includes(document.activeElement?.tagName);
  if (editingText) return;

  if ((event.key === "Delete" || event.key === "Backspace") && selectedNoteId) {
    event.preventDefault();
    const current = currentClip();
    if (!current) return;
    const command = {
      op: "remove_note",
      track_id: current.track.id,
      clip_id: current.clip.id,
      note_id: selectedNoteId,
    };
    if (await action("apply_command", { command })) {
      selectedNoteId = null;
      setStatus("已删除音符");
      await refresh();
    }
    return;
  }

  const modifier = event.ctrlKey || event.metaKey;
  if (modifier && event.key.toLowerCase() === "z") {
    event.preventDefault();
    await call(event.shiftKey ? "redo" : "undo");
    await refresh();
  } else if (modifier && event.key.toLowerCase() === "y") {
    event.preventDefault();
    await call("redo");
    await refresh();
  } else if (event.key === "Escape") {
    selectedNoteId = null;
    previewNote = null;
    draw();
  }
});

document.getElementById("play").addEventListener("click", async () => {
  if (await action("play")) {
    setStatus("准备播放");
    startPlaybackCursor();
  }
});

document.getElementById("add-track").addEventListener("click", async () => {
  const pianoTrackCount = project?.tracks.filter(
    (track) => track.source.kind === "midi" && track.source.instrument === "piano",
  ).length;
  const trackId = uniqueId("piano");
  const name = `Piano ${(pianoTrackCount || 0) + 1}`;
  if (
    await action("apply_command", {
      command: {
        op: "create_track",
        track_id: trackId,
        name,
        instrument: "piano",
      },
    })
  ) {
    activeTrack = trackId;
    selectedNoteId = null;
    previewNote = null;
    setStatus("已新增钢琴轨");
    await refresh();
  }
});

document.getElementById("stop").addEventListener("click", async () => {
  if (await action("stop")) {
    setStatus("已停止");
    stopPlaybackCursor();
  }
});

document.getElementById("new-project").addEventListener("click", async () => {
  if (workspace?.dirty && !window.confirm("当前工程有未保存修改，仍要新建工程吗？")) return;
  await action("stop", {});
  openProjectModal("new");
});

document.getElementById("open-project").addEventListener("click", async () => {
  if (workspace?.dirty && !window.confirm("当前工程有未保存修改，仍要打开吗？")) return;
  await action("stop", {});
  setStatus("正在打开工程");
  if (await action("load_project", {})) {
    stopPlaybackCursor();
    selectedNoteId = null;
    setStatus("已打开");
    await refresh();
  }
});

document.getElementById("save-project").addEventListener("click", async () => {
  if (workspace?.path) {
    if (await action("save_project", {})) {
      setStatus("已保存");
      await refresh();
    }
  } else {
    openProjectModal("save");
  }
});

document.getElementById("choose-piano-asset").addEventListener("click", async () => {
  if (!workspace?.path) {
    setStatus("请先创建或保存工程");
    return;
  }
  await action("stop", {});
  setStatus("正在加载钢琴音色");
  if (await action("choose_piano_asset", {})) {
    stopPlaybackCursor();
    setStatus("已绑定工程钢琴音色");
    await refresh();
  }
});

document.getElementById("render-project").addEventListener("click", async () => {
  setStatus("正在导出 WAV");
  if (await action("render_preview", {})) setStatus("已导出到工程 renders");
});

runAutopilotButton.addEventListener("click", async () => {
  const instruction = aiInstruction.value.trim();
  if (!instruction) {
    setStatus("请先描述想创作或调整什么");
    aiInstruction.focus();
    return;
  }
  await action("stop", {});
  runAutopilotButton.disabled = true;
  aiInstruction.disabled = true;
  aiResult.textContent = "AI 正在创作、渲染、独立评审并自动修订…";
  setStatus("AI 自动创作中");
  const outcome = await call("run_autopilot", { instruction });
  runAutopilotButton.disabled = false;
  aiInstruction.disabled = false;
  if (!outcome) {
    aiResult.textContent = "自动创作未完成，请查看底部错误信息。工程没有被部分提交。";
    return;
  }
  aiResult.textContent = `${outcome.evaluator_summary}（revision ${outcome.final_revision}，评审 ${outcome.evaluator_rounds} 轮）`;
  aiInstruction.value = "";
  setStatus(outcome.status === "completed" ? "AI 自动创作完成" : "AI 已完成当前自动修订上限");
  selectedNoteId = null;
  previewNote = null;
  await refresh();
});

aiInstruction.addEventListener("keydown", (event) => {
  if ((event.ctrlKey || event.metaKey) && event.key === "Enter") {
    event.preventDefault();
    runAutopilotButton.click();
  }
});

document.getElementById("undo").addEventListener("click", async () => {
  await call("undo");
  await refresh();
});

document.getElementById("redo").addEventListener("click", async () => {
  await call("redo");
  await refresh();
});

tempoInput.addEventListener("change", async () => {
  const bpm = Number(tempoInput.value);
  if (await action("apply_command", { command: { op: "set_tempo", tick: 0, bpm } })) {
    setStatus("已修改速度");
    await refresh();
  }
});

async function applyMeter() {
  const numerator = Number(meterNumerator.value);
  const denominator = Number(meterDenominator.value);
  if (await action("apply_command", {
    command: { op: "set_time_signature", numerator, denominator },
  })) {
    setStatus("已修改拍号");
    await refresh();
  } else {
    await refresh();
  }
}

meterNumerator.addEventListener("change", applyMeter);
meterDenominator.addEventListener("change", applyMeter);

document.getElementById("quantize").addEventListener("click", async () => {
  const current = currentClip();
  if (!current) return;
  const endTick = Math.max(
    current.clip.length_tick || 0,
    ...(current.clip.notes || []).map(
      (note) => note.start_tick + note.duration_tick,
    ),
    ...(current.clip.controls || []).map((control) => control.tick + 1),
  );
  if (endTick <= 0) return;
  if (await action("apply_command", {
    command: {
      op: "quantize_notes",
      track_id: current.track.id,
      clip_id: current.clip.id,
      start_tick: 0,
      end_tick: endTick,
      grid_tick: gridTick(),
      strength: Number(quantizeStrength.value),
    },
  })) {
    setStatus("已量化当前音轨");
    await refresh();
  }
});

window.addEventListener("resize", resizeCanvas);
editor.addEventListener("scroll", positionEmptyState, { passive: true });
resizeCanvas();
refresh();

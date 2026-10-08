const { invoke } = window.__TAURI__.core;
const status = document.getElementById('status');

function say(text, error = false) {
  status.textContent = text;
  status.className = error ? 'error' : '';
}

async function save(row, port) {
  const label = row.querySelector('input[type=text]').value;
  const hidden = row.querySelector('input[type=checkbox]').checked;
  row.classList.toggle('hidden', hidden);
  try {
    await invoke('save_port', { code: port.code, label, hidden });
    say('Saved');
  } catch (e) {
    say(String(e), true);
  }
}

const SYMBOLS = { Ctrl: '⌃', Alt: '⌥', Shift: '⇧', Cmd: '⌘' };
const KEY_NAMES = { Equal: '=', Minus: '-', Comma: ',', Period: '.', Slash: '/',
  Backslash: '\\', Semicolon: ';', Quote: "'", BracketLeft: '[', BracketRight: ']',
  Backquote: '`', Space: 'Space', ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→' };

// "Ctrl+Alt+Cmd+Equal" → "⌃⌥⌘="
function pretty(hotkey) {
  if (!hotkey) return 'Record';
  return hotkey.split('+').map(k => SYMBOLS[k] ?? KEY_NAMES[k] ?? k).join('');
}

// A keydown → "Ctrl+Alt+Cmd+Equal", or null while only modifiers are down.
function hotkeyFrom(e) {
  if (['Control', 'Alt', 'Shift', 'Meta'].includes(e.key)) return null;
  const mods = [];
  if (e.ctrlKey) mods.push('Ctrl');
  if (e.altKey) mods.push('Alt');
  if (e.shiftKey) mods.push('Shift');
  if (e.metaKey) mods.push('Cmd');
  const key = e.code.replace(/^Key/, '').replace(/^Digit/, '');
  return [...mods, key].join('+');
}

let recording = null;

function showHotkey(button, port) {
  button.textContent = pretty(port.hotkey);
}

// Ends a recording; `save` is the new hotkey to store first, if any.
// Hotkeys stay paused until the save is done, then resume once.
async function stopRecording(save) {
  if (!recording) return;
  const { button, port, onKey } = recording;
  recording = null;
  window.removeEventListener('keydown', onKey, true);
  button.classList.remove('recording');
  if (save) await setHotkey(port, save);
  showHotkey(button, port);
  await invoke('resume_hotkeys');
}

async function setHotkey(port, hotkey) {
  try {
    await invoke('set_hotkey', { code: port.code, hotkey });
    port.hotkey = hotkey;
    say(hotkey ? `Saved ${pretty(hotkey)}` : 'Hotkey cleared');
  } catch (e) {
    say(String(e), true);
  }
}

async function record(button, port) {
  await stopRecording();
  // Set the state before any await, so a blur during the pause still ends it.
  const onKey = (e) => keyDuringRecording(e);
  recording = { button, port, onKey };
  window.addEventListener('keydown', onKey, true);
  button.classList.add('recording');
  button.textContent = 'Press keys…';
  say('Press the new hotkey with at least one modifier. Esc cancels.');
  await invoke('pause_hotkeys');
}

async function keyDuringRecording(e) {
  e.preventDefault();
  e.stopPropagation();
  if (e.key === 'Escape') {
    await stopRecording();
    say('');
    return;
  }
  const hotkey = hotkeyFrom(e);
  if (!hotkey) return;
  if (!(e.ctrlKey || e.altKey || e.metaKey)) {
    say('Use at least one of ⌃, ⌥ or ⌘.', true);
    return;
  }
  await stopRecording(hotkey);
}

window.addEventListener('blur', () => stopRecording());

async function load() {
  try {
    const s = await invoke('get_settings');
    document.getElementById('monitor').textContent = s.monitor;
    const body = document.getElementById('ports');
    body.replaceChildren();
    for (const port of s.ports) {
      const row = document.createElement('tr');
      row.classList.toggle('hidden', port.hidden);
      row.innerHTML = '<td class="name"></td><td><input type="text"></td>' +
        '<td class="hotkey"><button class="record"></button><button class="clear" title="Clear hotkey">×</button></td>' +
        '<td><input type="checkbox"></td><td class="code"></td><td class="remove"></td>';
      row.querySelector('.name').textContent = port.name;
      if (!port.detected) {
        const tag = document.createElement('span');
        tag.className = 'manual';
        tag.textContent = 'added';
        row.querySelector('.name').append(tag);
        const remove = document.createElement('button');
        remove.textContent = 'Remove';
        remove.addEventListener('click', () => removePort(port));
        row.querySelector('.remove').append(remove);
      }
      row.querySelector('.code').textContent = port.code;
      const label = row.querySelector('input[type=text]');
      label.value = port.label ?? '';
      label.placeholder = port.name;
      // Save shortly after typing stops, so closing the window keeps the edit.
      let timer;
      label.addEventListener('input', () => {
        clearTimeout(timer);
        timer = setTimeout(() => save(row, port), 400);
      });
      const recordButton = row.querySelector('button.record');
      showHotkey(recordButton, port);
      recordButton.addEventListener('click', () => record(recordButton, port));
      row.querySelector('button.clear').addEventListener('click', async () => {
        await stopRecording();
        await setHotkey(port, null);
        showHotkey(recordButton, port);
      });
      const hide = row.querySelector('input[type=checkbox]');
      hide.checked = port.hidden;
      hide.addEventListener('change', () => save(row, port));
      body.append(row);
    }
    fillStandardInputs(s.standard_inputs);
  } catch (e) {
    say(String(e), true);
  }
}

function fillStandardInputs(inputs) {
  const select = document.getElementById('standard');
  if (select.options.length > 1) return;
  for (const [code, name] of inputs) {
    const option = document.createElement('option');
    option.value = code;
    option.textContent = `${name} (${code})`;
    select.append(option);
  }
}

document.getElementById('standard').addEventListener('change', (e) => {
  document.getElementById('code').value = e.target.value;
});

document.getElementById('add').addEventListener('click', async () => {
  const field = document.getElementById('code');
  try {
    const added = await invoke('add_port', { code: field.value });
    field.value = '';
    document.getElementById('standard').value = '';
    say(`Added ${added}`);
    await load();
  } catch (e) {
    say(String(e), true);
  }
});

async function removePort(port) {
  try {
    await invoke('remove_port', { code: port.code });
    say('Port removed');
    await load();
  } catch (e) {
    say(String(e), true);
  }
}

load();

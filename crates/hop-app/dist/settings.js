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
        '<td><input type="checkbox"></td><td class="code"></td>';
      row.querySelector('.name').textContent = port.name;
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
      const hide = row.querySelector('input[type=checkbox]');
      hide.checked = port.hidden;
      hide.addEventListener('change', () => save(row, port));
      body.append(row);
    }
  } catch (e) {
    say(String(e), true);
  }
}

load();

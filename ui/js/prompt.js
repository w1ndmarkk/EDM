// Question window (title, message, buttons, optional checkbox); the answer goes back to the core.
'use strict';

const promptId = Number(new URLSearchParams(location.search).get('id'));
let spec = null;
const answer = (button) => invoke('prompt_answer', { id: promptId, button, checked: $('#check').checked });

(async () => {
  const b = await invoke('bootstrap');
  await initI18n(b.lang);
  spec = await invoke('prompt_get', { id: promptId });
  if (!spec) { TAURI.window.getCurrentWindow().close(); return; }
  $('#title').textContent = spec.title;
  $('#message').textContent = spec.message;
  $('#message').classList.toggle('hidden', !spec.message);
  if (spec.detail) { $('#detail').textContent = spec.detail; $('#detail').classList.remove('hidden'); }
  if (spec.checkbox) { $('#checkText').textContent = spec.checkbox; $('#checkRow').classList.remove('hidden'); }
  spec.buttons.forEach((text, i) => {
    $('#buttons').append(el('button', { class: 'btn' + (i === spec.defaultButton ? ' primary' : ''), text, onclick: () => answer(i) }));
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') { e.preventDefault(); answer(spec.defaultButton); }
    if (e.key === 'Escape') { e.preventDefault(); answer(spec.cancelButton ?? null); }
  });
  // Size the window to its content, then show it.
  requestAnimationFrame(async () => {
    const h = Math.ceil(document.body.scrollHeight);
    await invoke('window_fit', { width: 500, height: Math.min(h, 640), show: true });
    const def = $('#buttons').children[spec.defaultButton];
    if (def) def.focus();
  });
})();

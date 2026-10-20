// Transport to the engine. In the desktop app this is Tauri's IPC; in the
// browser preview it is a token-protected POST to the local preview server.
// Both reach the same `Api::call` in twinpane-core.

const tauri = window.__TAURI__;

export const isPreview = !(tauri && tauri.core && tauri.core.invoke);

function previewToken() {
  const m = document.querySelector('meta[name="twinpane-token"]');
  return m ? m.content : '';
}

export async function call(cmd, args = {}) {
  if (!isPreview) {
    try {
      return await tauri.core.invoke('call', { cmd, args });
    } catch (e) {
      throw new Error(typeof e === 'string' ? e : (e && e.message) || String(e));
    }
  }
  let res;
  try {
    res = await fetch(`/api/${cmd}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-twinpane-token': previewToken() },
      body: JSON.stringify(args),
    });
  } catch {
    throw new Error('Could not reach the preview server. Is it still running?');
  }
  let body = null;
  try { body = await res.json(); } catch { /* handled below */ }
  if (!res.ok || !body || body.error) {
    throw new Error((body && body.error) || `Request failed (${res.status})`);
  }
  return body.ok;
}

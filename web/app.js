const $ = (id) => document.getElementById(id);
const state = { revision: 0, messages: [], clipboard: null, files: [] };

function setStatus(text, kind = "loading") { const el = $("status"); el.textContent = text; el.dataset.kind = kind; }
function escapeText(value) { return String(value ?? ""); }
function formatDate(value) { try { return new Date(value).toLocaleString(); } catch { return value; } }
function formatSize(bytes) { if (bytes < 1024) return `${bytes} B`; if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`; if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`; return `${(bytes / 1024 ** 3).toFixed(1)} GB`; }

async function request(path, options = {}) { const response = await fetch(path, { ...options, headers: { ...(options.body instanceof FormData ? {} : { "Content-Type": "application/json" }), ...(options.headers || {}) } }); const body = await response.json().catch(() => ({})); if (!response.ok) throw new Error(body.error?.message || `请求失败 (${response.status})`); return body; }

function render() {
  const clip = state.clipboard;
  $("clipboard-text").value = clip?.text || "";
  $("clipboard-time").textContent = clip ? `${clip.source} · ${formatDate(clip.updated_at)}` : "尚无内容";
  const messages = $("messages"); messages.replaceChildren();
  for (const item of state.messages) { const wrapper = document.createElement("article"); wrapper.className = "message"; const meta = document.createElement("div"); meta.className = "message-meta"; const author = document.createElement("span"); author.textContent = item.author; const date = document.createElement("span"); date.textContent = formatDate(item.created_at); meta.append(author, date); const text = document.createElement("p"); text.textContent = item.text; const remove = document.createElement("button"); remove.className = "danger"; remove.textContent = "删除"; remove.onclick = () => removeMessage(item.id); meta.append(remove); wrapper.append(meta, text); messages.append(wrapper); }
  const files = $("files"); files.replaceChildren();
  for (const item of state.files) { const row = document.createElement("div"); row.className = "file"; const detail = document.createElement("div"); detail.className = "file-name"; const name = document.createElement("strong"); name.textContent = item.name; const info = document.createElement("div"); info.className = "file-info"; info.textContent = `${formatSize(item.size)} · ${formatDate(item.created_at)}`; detail.append(name, info); const actions = document.createElement("div"); actions.className = "file-actions"; const link = document.createElement("a"); link.href = `/api/files/${encodeURIComponent(item.id)}`; link.textContent = "下载"; link.download = item.name; const remove = document.createElement("button"); remove.className = "danger"; remove.textContent = "删除"; remove.onclick = () => removeFile(item.id); actions.append(link, remove); row.append(detail, actions); files.append(row); }
}

async function refresh() { const next = await request("/api/state"); Object.assign(state, next); state.messages.sort((a, b) => b.created_at.localeCompare(a.created_at)); state.files.sort((a, b) => b.created_at.localeCompare(a.created_at)); render(); setStatus("已连接", "ok"); }
async function removeMessage(id) { if (!confirm("删除这条消息？")) return; try { await request(`/api/messages/${encodeURIComponent(id)}`, { method: "DELETE" }); await refresh(); } catch (error) { alert(error.message); } }
async function removeFile(id) { if (!confirm("删除这个文件？")) return; try { await request(`/api/files/${encodeURIComponent(id)}`, { method: "DELETE" }); await refresh(); } catch (error) { alert(error.message); } }

$("publish-clipboard").onclick = async () => { try { const item = await request("/api/clipboard", { method: "POST", body: JSON.stringify({ text: $("clipboard-text").value, source: $("clipboard-source").value || "浏览器设备" }) }); state.clipboard = item; render(); } catch (error) { alert(error.message); } };
$("copy-clipboard").onclick = async () => { try { await navigator.clipboard.writeText($("clipboard-text").value); setStatus("已复制到本机剪贴板", "ok"); } catch { alert("浏览器拒绝访问剪贴板，请手动选择并复制文本。") } };
$("message-form").onsubmit = async (event) => { event.preventDefault(); try { await request("/api/messages", { method: "POST", body: JSON.stringify({ text: $("message-text").value, author: $("message-author").value || "浏览器设备" }) }); $("message-text").value = ""; await refresh(); } catch (error) { alert(error.message); } };
async function uploadOne(file) {
  const form = new FormData(); form.append("file", file);
  await new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest(); xhr.open("POST", "/api/files");
    xhr.upload.onprogress = (event) => { if (event.lengthComputable) $("file-limit").textContent = `上传中 ${Math.round(event.loaded / event.total * 100)}%`; };
    xhr.onerror = () => reject(new Error("网络连接失败"));
    xhr.onload = () => { let body = {}; try { body = JSON.parse(xhr.responseText); } catch (_) {} if (xhr.status >= 200 && xhr.status < 300) resolve(body); else reject(new Error(body.error?.message || `上传失败 (${xhr.status})`)); };
    xhr.send(form);
  });
}
async function upload(files) { for (const file of files) { try { await uploadOne(file); } catch (error) { alert(`${file.name}: ${error.message}`); } } $("file-limit").textContent = "上传到此设备"; await refresh(); }
$("file-input").onchange = (event) => upload(event.target.files);
$("dropzone").ondragover = (event) => { event.preventDefault(); $("dropzone").classList.add("drag"); };
$("dropzone").ondragleave = () => $("dropzone").classList.remove("drag");
$("dropzone").ondrop = (event) => { event.preventDefault(); $("dropzone").classList.remove("drag"); upload(event.dataTransfer.files); };

function subscribe() { const source = new EventSource(`/api/events?since=${state.revision}`); source.onmessage = () => refresh().catch(() => setStatus("同步失败，重试中…", "error")); source.onerror = () => { source.close(); setStatus("连接断开，重试中…", "error"); setTimeout(subscribe, 2500); }; }
refresh().then(subscribe).catch(() => { setStatus("无法连接服务", "error"); setTimeout(() => location.reload(), 5000); });

const toast = document.querySelector('#toast')
const title = document.querySelector('#title')
const message = document.querySelector('#message')
const meta = document.querySelector('#meta')
const stepsEl = document.querySelector('#steps')
const bar = document.querySelector('#bar')
const fill = document.querySelector('#fill')
const invoke = window.__TAURI__?.core?.invoke

const BUSY = new Set([
  'checking',
  'downloading',
  'verifying',
  'staging',
  'switching',
  'rolling_back',
  'plugin_installing',
  'plugin_removing',
  'plugin_toggling',
  'installing',
])

const TITLES = {
  checking: '正在检查更新',
  downloading: '正在安装',
  verifying: '正在校验',
  staging: '正在写入',
  switching: '正在切换',
  rolling_back: '正在回退',
  plugin_installing: '正在添加插件',
  plugin_removing: '正在删除插件',
  plugin_toggling: '正在切换插件',
  installing: '正在安装桌面更新',
  available: '发现新版本',
  failed: '失败',
  idle: '更新',
}

let hideTimer = null
let lastHideKey = ''

function formatBytes(bytes) {
  return `${(bytes / 1_048_576).toFixed(1)} MB`
}

function currentDetail(steps) {
  const active = [...steps].reverse().find((step) => step.status === 'active' || step.status === 'failed')
  const done = [...steps].reverse().find((step) => step.status === 'done')
  return active?.detail || done?.detail || ''
}

function renderSteps(steps) {
  stepsEl.replaceChildren()
  stepsEl.hidden = !steps.length
  for (const step of steps) {
    const item = document.createElement('li')
    item.className = `toast-step ${step.status || 'pending'}`
    item.textContent = step.label
    stepsEl.append(item)
  }
}

function render(snapshot) {
  const phase = snapshot.update_phase || 'idle'
  const read = snapshot.update_bytes_read || 0
  const total = snapshot.update_bytes_total || 0
  const busy = BUSY.has(phase)
  const steps = snapshot.update_steps || []
  const detail = currentDetail(steps)
  const doneCount = steps.filter((step) => step.status === 'done').length

  title.textContent =
    phase === 'plugin_toggling'
      ? (snapshot.update_message || '').includes('启用')
        ? '正在启用插件'
        : '正在停用插件'
      : TITLES[phase] || TITLES.idle
  message.textContent = detail || snapshot.update_message || '正在准备。'
  toast.classList.toggle('failed', phase === 'failed')
  toast.classList.toggle('available', phase === 'available')
  toast.classList.toggle('busy', busy)
  renderSteps(steps)

  bar.classList.toggle('indeterminate', busy && !(phase === 'downloading' && total > 0))

  if (phase === 'downloading' && total > 0) {
    const percent = Math.max(0, Math.min(100, (read / total) * 100))
    fill.style.width = `${percent}%`
    meta.textContent = `${Math.round(percent)}%`
  } else if (phase === 'downloading') {
    fill.style.width = '36%'
    meta.textContent = formatBytes(read)
  } else if (busy && steps.length) {
    fill.style.width = `${Math.max(12, (doneCount / steps.length) * 100)}%`
    meta.textContent = `${Math.min(doneCount + 1, steps.length)}/${steps.length}`
  } else if (busy) {
    fill.style.width = '36%'
    meta.textContent = ''
  } else if (phase === 'failed') {
    fill.style.width = '100%'
    meta.textContent = ''
  } else {
    fill.style.width = '100%'
    meta.textContent = phase === 'available' ? snapshot.available_version || '' : '完成'
  }

  const hideKey = `${phase}|${snapshot.update_message}`
  if (hideKey === lastHideKey) {
    return
  }
  lastHideKey = hideKey
  window.clearTimeout(hideTimer)
  hideTimer = null
  if (!busy) {
    const delay = phase === 'failed' || phase === 'available' ? 10000 : 8000
    hideTimer = window.setTimeout(() => {
      invoke?.('dismiss_update_toast')
    }, delay)
  }
}

async function refresh() {
  if (!invoke) {
    return
  }
  try {
    render(await invoke('runtime_status'))
  } catch {
    title.textContent = '无法读取更新状态'
  }
}

toast.addEventListener('click', () => {
  if (!toast.classList.contains('busy')) {
    invoke?.('dismiss_update_toast')
  }
})

refresh()
setInterval(refresh, 300)

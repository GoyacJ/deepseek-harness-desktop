const title = document.querySelector('#title')
const message = document.querySelector('#message')
const packageName = document.querySelector('#package')
const pid = document.querySelector('#pid')
const retry = document.querySelector('#retry')
const diagnostics = document.querySelector('#diagnostics')
const logs = document.querySelector('#logs')

const invoke = window.__TAURI__?.core?.invoke

function render(snapshot) {
  const phase = snapshot.phase
  packageName.textContent = snapshot.package_spec
  pid.textContent = snapshot.pid ? `PID ${snapshot.pid}` : '等待进程'
  message.textContent = snapshot.message
  logs.textContent = snapshot.recent_logs.join('\n')
  diagnostics.hidden = snapshot.recent_logs.length === 0

  if (phase === 'failed') {
    title.textContent = 'DSH 启动失败'
    retry.hidden = false
    retry.disabled = false
  } else if (phase === 'ready') {
    title.textContent = 'DSH 已就绪'
    retry.hidden = true
  } else if (phase === 'stopping') {
    title.textContent = '正在关闭 DSH'
    retry.hidden = true
  } else {
    title.textContent = '正在启动官方 DSH'
    retry.hidden = true
  }
}

async function refresh() {
  if (!invoke) {
    title.textContent = 'Tauri 运行时不可用'
    message.textContent = '请通过 Tauri 启动本应用。'
    return
  }

  try {
    render(await invoke('runtime_status'))
  } catch (error) {
    title.textContent = '无法读取运行状态'
    message.textContent = String(error)
  }
}

retry.addEventListener('click', async () => {
  retry.disabled = true
  await invoke('restart_runtime')
  await refresh()
})

refresh()
setInterval(refresh, 400)

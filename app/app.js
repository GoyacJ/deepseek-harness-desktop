const title = document.querySelector('#title')
const message = document.querySelector('#message')
const packageName = document.querySelector('#package')
const version = document.querySelector('#version')
const source = document.querySelector('#source')
const pid = document.querySelector('#pid')
const update = document.querySelector('#update')
const retry = document.querySelector('#retry')
const diagnostics = document.querySelector('#diagnostics')
const logs = document.querySelector('#logs')

const invoke = window.__TAURI__?.core?.invoke

function render(snapshot) {
  const phase = snapshot.phase
  packageName.textContent = snapshot.package_spec
  version.textContent = `DSH ${snapshot.dsh_version ?? ''}`
  source.textContent = snapshot.runtime_source ?? 'bundled'
  pid.textContent = snapshot.pid ? `PID ${snapshot.pid}` : '等待进程'
  message.textContent = snapshot.message
  logs.textContent = snapshot.recent_logs.join('\n')
  diagnostics.hidden = snapshot.recent_logs.length === 0

  const updateMessage = snapshot.update_message
  const updatePhase = snapshot.update_phase
  if (updateMessage && updatePhase && updatePhase !== 'idle') {
    update.hidden = false
    update.textContent = updateMessage
  } else if (updateMessage) {
    update.hidden = false
    update.textContent = updateMessage
  } else {
    update.hidden = true
    update.textContent = ''
  }

  if (updatePhase === 'switching' || updatePhase === 'rolling_back') {
    title.textContent = updatePhase === 'switching' ? '正在切换 DSH' : '正在回退 DSH'
    retry.hidden = true
  } else if (updatePhase === 'installing') {
    title.textContent = '正在安装桌面更新'
    retry.hidden = true
  } else if (phase === 'failed') {
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

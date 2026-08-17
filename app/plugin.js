const invoke = window.__TAURI__?.core?.invoke
const form = document.querySelector('#form')
const spec = document.querySelector('#spec')
const error = document.querySelector('#error')
const submit = document.querySelector('#submit')
const done = document.querySelector('#done')
const list = document.querySelector('#plugin-list')
const empty = document.querySelector('#plugin-empty')
const installedPane = document.querySelector('#installed-pane')
const marketPane = document.querySelector('#market-pane')
const marketList = document.querySelector('#market-list')
const marketState = document.querySelector('#market-state')
const marketStatus = document.querySelector('#market-status')
const marketRetry = document.querySelector('#market-retry')
const marketQuery = document.querySelector('#market-query')
const chips = document.querySelectorAll('.plugin-chip')
const sorts = document.querySelectorAll('.plugin-sort-btn')
const tabs = document.querySelectorAll('.plugin-tab')
const confirmEl = document.querySelector('#confirm')
const confirmTitle = document.querySelector('#confirm-title')
const confirmMessage = document.querySelector('#confirm-message')
const confirmOk = document.querySelector('#confirm-ok')
const confirmCancel = document.querySelector('#confirm-cancel')
const confirmClose = document.querySelector('#confirm-close')
const confirmDismiss = document.querySelector('#confirm-dismiss')
const confirmCard = document.querySelector('.plugin-confirm-card')
const installedCount = document.querySelector('#installed-count')

let currentTab = 'installed'
let confirmResolver = null
let confirmReturnFocus = null
let installed = []
let marketItems = []
let marketTotal = 0
let marketTimer = 0
let marketLoading = false
let marketCategory = ''
let marketSort = 'trending'
let expandedKey = ''
const detailCache = new Map()

function showError(message) {
  error.hidden = !message
  error.textContent = message || ''
}

function markFor(name) {
  const last = (name.split('/').pop() || name).replace(/^@/, '')
  const parts = last.split(/[-_.]/).filter(Boolean)
  if (parts.length >= 2) {
    return (parts[0][0] + parts[1][0]).toUpperCase()
  }
  return last.slice(0, 2).toUpperCase()
}

function setTab(tab) {
  currentTab = tab
  for (const button of tabs) {
    const active = button.dataset.tab === tab
    button.classList.toggle('active', active)
    button.setAttribute('aria-selected', String(active))
  }
  installedPane.hidden = tab !== 'installed'
  marketPane.hidden = tab !== 'market'
  if (tab === 'market' && !marketItems.length && !marketLoading) {
    loadMarket(true)
  }
}

function hideConfirm(ok) {
  confirmEl.hidden = true
  const resolve = confirmResolver
  confirmResolver = null
  resolve?.(ok)
  confirmReturnFocus?.focus()
  confirmReturnFocus = null
}

function askConfirm(title, message, options = {}) {
  if (confirmResolver) {
    hideConfirm(false)
  }
  confirmReturnFocus = document.activeElement
  confirmTitle.textContent = title
  confirmMessage.textContent = message
  confirmOk.textContent = options.label || '确定'
  confirmOk.classList.toggle('plugin-button-primary', !options.danger)
  confirmOk.classList.toggle('plugin-button-danger', Boolean(options.danger))
  confirmCard.classList.toggle('danger', Boolean(options.danger))
  confirmEl.hidden = false
  confirmOk.focus()
  return new Promise((resolve) => {
    confirmResolver = resolve
  })
}

function closeDialog() {
  if (!confirmEl.hidden) {
    hideConfirm(false)
  }
  spec.value = ''
  showError('')
  invoke?.('dismiss_plugin_dialog')
}

function renderInstalled() {
  list.replaceChildren()
  installedCount.textContent = String(installed.length)
  list.hidden = installed.length === 0
  empty.hidden = installed.length > 0
  for (const plugin of installed) {
    const row = document.createElement('div')
    row.className = plugin.enabled ? 'plugin-row' : 'plugin-row plugin-row-off'
    const mark = document.createElement('span')
    mark.className = 'plugin-mark'
    mark.textContent = markFor(plugin.name)
    const meta = document.createElement('div')
    meta.className = 'plugin-row-main'
    const name = document.createElement('p')
    name.className = 'plugin-row-name'
    name.textContent = plugin.name
    const detail = document.createElement('p')
    detail.className = 'plugin-row-meta'
    const version = document.createElement('span')
    version.textContent = plugin.version || '未知版本'
    const sep = document.createTextNode(' · ')
    const status = document.createElement('span')
    status.className = plugin.enabled ? 'plugin-on' : 'plugin-off'
    status.textContent = plugin.enabled ? '已启用' : '已停用'
    detail.append(version, sep, status)
    meta.append(name, detail)
    const actions = document.createElement('div')
    actions.className = 'plugin-row-actions'
    const toggle = document.createElement('button')
    toggle.className = 'plugin-switch'
    toggle.type = 'button'
    toggle.setAttribute('role', 'switch')
    toggle.setAttribute('aria-checked', String(plugin.enabled))
    toggle.setAttribute('aria-label', plugin.enabled ? `停用 ${plugin.name}` : `启用 ${plugin.name}`)
    toggle.title = plugin.enabled ? '停用插件' : '启用插件'
    toggle.textContent = plugin.enabled ? '停用' : '启用'
    toggle.addEventListener('click', async () => {
      const enabled = !plugin.enabled
      const action = enabled ? '启用' : '停用'
      const extra = enabled ? '' : '包仍保留，可随时启用。'
      const ok = await askConfirm(
        `${action}插件`,
        `${plugin.name} 将被${action}。${extra}DSH 随后会自动重启。`,
        { label: action },
      )
      if (!ok) {
        return
      }
      invoke?.('submit_plugin_set_enabled', {
        spec: plugin.name,
        enabled,
      })
    })
    const remove = document.createElement('button')
    remove.className = 'plugin-text plugin-remove'
    remove.type = 'button'
    remove.textContent = '删除'
    remove.addEventListener('click', async () => {
      const ok = await askConfirm(
        '删除插件',
        `${plugin.name} 将从当前环境卸载，DSH 随后会自动重启。`,
        { label: '删除', danger: true },
      )
      if (!ok) {
        return
      }
      invoke?.('submit_plugin_remove', { spec: plugin.name })
    })
    actions.append(toggle, remove)
    row.append(mark, meta, actions)
    list.append(row)
  }
}

function isInstalled(item) {
  const repo = item.name.toLowerCase()
  return installed.some((plugin) => {
    const name = plugin.name.toLowerCase()
    return name === repo || name.endsWith(`/${repo}`)
  })
}

function showMarketState(message, retry) {
  marketState.hidden = false
  marketStatus.textContent = message
  marketRetry.hidden = !retry
}

function hideMarketState() {
  marketState.hidden = true
  marketRetry.hidden = true
}

function renderSkeleton() {
  marketList.replaceChildren()
  marketList.hidden = false
  hideMarketState()
  for (let index = 0; index < 3; index += 1) {
    const row = document.createElement('div')
    row.className = 'plugin-row plugin-skeleton'
    const mark = document.createElement('span')
    mark.className = 'plugin-mark'
    const meta = document.createElement('div')
    meta.className = 'plugin-row-main'
    const name = document.createElement('p')
    name.className = 'plugin-row-name'
    const detail = document.createElement('p')
    detail.className = 'plugin-row-meta wrap'
    meta.append(name, detail)
    row.append(mark, meta)
    marketList.append(row)
  }
}

function renderMarket(append) {
  if (!append) {
    marketList.replaceChildren()
  }
  const start = append ? marketList.querySelectorAll('.plugin-row:not(.plugin-skeleton)').length : 0
  const next = marketItems.slice(start)
  for (const item of next) {
    const key = pluginKey(item)
    const open = key === expandedKey
    const row = document.createElement('div')
    row.className = open ? 'plugin-row plugin-row-market is-open' : 'plugin-row plugin-row-market'
    row.dataset.key = key
    row.tabIndex = 0
    row.setAttribute('role', 'button')
    row.setAttribute('aria-expanded', String(open))
    const mark = document.createElement('span')
    mark.className = 'plugin-mark'
    mark.textContent = markFor(item.name)
    const meta = document.createElement('div')
    meta.className = 'plugin-row-main'
    const title = document.createElement('div')
    title.className = 'plugin-row-title'
    const name = document.createElement('p')
    name.className = 'plugin-row-name'
    name.textContent = item.full_name || `${item.owner}/${item.name}`
    const caret = document.createElement('span')
    caret.className = 'plugin-caret'
    caret.setAttribute('aria-hidden', 'true')
    caret.innerHTML =
      '<svg viewBox="0 0 12 12" width="8" height="8"><path d="M4.2 2.8 7.4 6 4.2 9.2" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/></svg>'
    title.append(name, caret)
    if (item.category) {
      const chip = document.createElement('span')
      chip.className = 'plugin-tag'
      chip.textContent = item.category
      title.append(chip)
    }
    const detail = document.createElement('p')
    detail.className = 'plugin-row-meta wrap'
    detail.textContent = item.description || '暂无简介'
    const facts = document.createElement('div')
    facts.className = 'plugin-market-meta'
    const publisher = document.createElement('span')
    publisher.textContent = item.owner
    const stars = document.createElement('span')
    stars.className = 'plugin-stars'
    stars.innerHTML =
      '<svg viewBox="0 0 16 16" aria-hidden="true"><path d="m8 2 1.75 3.55 3.92.57-2.84 2.76.67 3.9L8 10.94l-3.5 1.84.67-3.9-2.84-2.76 3.92-.57L8 2Z" fill="none" stroke="currentColor" stroke-linejoin="round"/></svg>'
    stars.append(document.createTextNode(String(item.stars ?? 0)))
    facts.append(publisher, stars)
    meta.append(title, detail, facts)
    const actions = document.createElement('div')
    actions.className = 'plugin-row-actions plugin-row-actions-col'
    const add = document.createElement('button')
    add.type = 'button'
    if (isInstalled(item)) {
      add.className = 'plugin-text plugin-text-faint'
      add.textContent = '已安装'
      add.disabled = true
    } else {
      add.className = 'plugin-button plugin-button-primary plugin-button-small'
      add.textContent = '安装'
      add.addEventListener('click', async (event) => {
        event.stopPropagation()
        const ok = await askConfirm(
          '安装插件',
          `${item.owner}/${item.name} 将安装到当前环境，DSH 随后会自动重启。`,
          { label: '安装' },
        )
        if (!ok) {
          return
        }
        add.disabled = true
        try {
          await invoke?.('submit_hub_install', { owner: item.owner, name: item.name })
        } catch (cause) {
          showMarketState(String(cause?.message || cause || '无法安装该插件'), false)
          add.disabled = false
        }
      })
    }
    actions.append(add)
    const panel = document.createElement('div')
    panel.className = 'plugin-detail'
    panel.hidden = !open
    if (open) {
      paintDetail(panel, detailCache.get(key) || item)
      if (!detailCache.has(key)) {
        loadMarketDetail(item, panel)
      }
    }
    row.append(mark, meta, actions, panel)
    row.addEventListener('click', (event) => {
      if (event.target.closest('button')) {
        return
      }
      toggleMarketItem(item)
    })
    row.addEventListener('keydown', (event) => {
      if (event.target !== row || (event.key !== 'Enter' && event.key !== ' ')) {
        return
      }
      event.preventDefault()
      toggleMarketItem(item)
    })
    marketList.append(row)
  }
  marketList.hidden = marketItems.length === 0
  if (!marketItems.length && !marketLoading) {
    showMarketState('没有找到已验证的插件。', false)
  } else {
    hideMarketState()
  }
}

function pluginKey(item) {
  return `${item.owner}/${item.name}`
}

function formatDay(value) {
  return value ? String(value).slice(0, 10) : ''
}

function appendFact(parent, label, value) {
  if (!value && value !== 0) {
    return
  }
  const item = document.createElement('span')
  item.className = 'plugin-fact'
  const name = document.createElement('em')
  name.textContent = label
  item.append(name, document.createTextNode(String(value)))
  parent.append(item)
}

function paintDetail(panel, data) {
  panel.replaceChildren()
  const facts = document.createElement('div')
  facts.className = 'plugin-facts'
  appendFact(facts, '包名', data.package_name)
  appendFact(facts, '版本', data.version)
  appendFact(facts, '许可', data.license)
  appendFact(facts, '更新', formatDay(data.pushed_at))
  if (data.forks) {
    appendFact(facts, 'Fork', data.forks)
  }
  if (facts.childNodes.length) {
    panel.append(facts)
  }
  const topics = data.topics || []
  if (topics.length) {
    const row = document.createElement('div')
    row.className = 'plugin-topics'
    for (const topic of topics.slice(0, 8)) {
      const tag = document.createElement('span')
      tag.className = 'plugin-tag'
      tag.textContent = topic
      row.append(tag)
    }
    panel.append(row)
  }
  const repo = data.repository_url || data.homepage
  if (repo) {
    const link = document.createElement('p')
    link.className = 'plugin-repo'
    link.textContent = String(repo).replace(/^https?:\/\//, '')
    panel.append(link)
  }
}

function toggleMarketItem(item) {
  const key = pluginKey(item)
  expandedKey = expandedKey === key ? '' : key
  for (const row of marketList.querySelectorAll('.plugin-row-market')) {
    const open = row.dataset.key === expandedKey
    row.classList.toggle('is-open', open)
    row.setAttribute('aria-expanded', String(open))
    const panel = row.querySelector('.plugin-detail')
    if (!panel) {
      continue
    }
    panel.hidden = !open
    if (open) {
      const cached = detailCache.get(key)
      paintDetail(panel, cached || item)
      if (!cached) {
        loadMarketDetail(item, panel)
      }
    }
  }
}

async function loadMarketDetail(item, panel) {
  const key = pluginKey(item)
  const status = document.createElement('p')
  status.className = 'plugin-detail-status'
  status.textContent = '正在加载详情'
  panel.append(status)
  try {
    const detail = await invoke?.('get_hub_plugin', {
      owner: item.owner,
      name: item.name,
    })
    if (!detail) {
      throw new Error('empty')
    }
    detailCache.set(key, detail)
    if (expandedKey === key) {
      paintDetail(panel, detail)
    }
  } catch {
    if (expandedKey === key) {
      status.textContent = '无法加载更多详情'
    }
  }
}

async function refreshInstalled() {
  try {
    installed = (await invoke?.('list_user_plugins')) || []
  } catch {
    installed = []
  }
  renderInstalled()
  if (currentTab === 'market' && marketItems.length) {
    renderMarket(false)
  }
}

async function loadMarket(reset) {
  if (marketLoading) {
    return
  }
  if (!reset && marketItems.length >= marketTotal && marketTotal > 0) {
    return
  }
  marketLoading = true
  if (reset) {
    marketItems = []
    marketTotal = 0
    renderSkeleton()
  }
  try {
    const page = await invoke?.('search_hub_plugins', {
      query: marketQuery.value.trim(),
      category: marketCategory,
      sort: marketSort,
      offset: reset ? 0 : marketItems.length,
    })
    const items = page?.items || []
    marketTotal = page?.total || 0
    if (reset) {
      marketItems = items
      renderMarket(false)
    } else {
      marketItems = marketItems.concat(items)
      renderMarket(true)
    }
  } catch {
    if (reset) {
      marketList.replaceChildren()
      marketList.hidden = true
    }
    showMarketState('无法加载市场', true)
  } finally {
    marketLoading = false
  }
}

function queueMarket() {
  window.clearTimeout(marketTimer)
  marketTimer = window.setTimeout(() => loadMarket(true), 280)
}

window.refreshAll = async function refreshAll() {
  if (!confirmEl.hidden) {
    hideConfirm(false)
  }
  showError('')
  spec.value = ''
  setTab('installed')
  await refreshInstalled()
}

form.addEventListener('submit', async (event) => {
  event.preventDefault()
  showError('')
  const value = spec.value.trim()
  if (!value) {
    showError('请输入 npm 包名。')
    spec.focus()
    return
  }
  const ok = await askConfirm(
    '安装 npm 插件',
    `${value} 将安装到当前 Web 环境，DSH 随后会自动重启。请确认你信任此包。`,
    { label: '安装' },
  )
  if (!ok) {
    return
  }
  submit.disabled = true
  try {
    await invoke?.('submit_plugin_add', { spec: value })
  } catch (cause) {
    showError(String(cause?.message || cause || '无法添加插件'))
    spec.focus()
  } finally {
    submit.disabled = false
  }
})

done.addEventListener('click', closeDialog)
confirmOk.addEventListener('click', () => hideConfirm(true))
confirmCancel.addEventListener('click', () => hideConfirm(false))
confirmClose.addEventListener('click', () => hideConfirm(false))
confirmDismiss.addEventListener('click', () => hideConfirm(false))

document.addEventListener('keydown', (event) => {
  if (event.key !== 'Escape') {
    return
  }
  if (!confirmEl.hidden) {
    hideConfirm(false)
    return
  }
  closeDialog()
})

for (const button of tabs) {
  button.addEventListener('click', () => setTab(button.dataset.tab))
}

for (const chip of chips) {
  chip.addEventListener('click', () => {
    marketCategory = chip.dataset.category || ''
    for (const item of chips) {
      item.classList.toggle('active', item === chip)
    }
    loadMarket(true)
  })
}

for (const button of sorts) {
  button.addEventListener('click', () => {
    marketSort = button.dataset.sort || 'trending'
    for (const item of sorts) {
      item.classList.toggle('active', item === button)
    }
    loadMarket(true)
  })
}

marketQuery.addEventListener('input', queueMarket)
marketRetry.addEventListener('click', () => loadMarket(true))
marketList.addEventListener('scroll', () => {
  if (marketLoading || marketItems.length >= marketTotal) {
    return
  }
  if (marketList.scrollTop + marketList.clientHeight > marketList.scrollHeight - 48) {
    loadMarket(false)
  }
})

refreshInstalled()
spec.focus()

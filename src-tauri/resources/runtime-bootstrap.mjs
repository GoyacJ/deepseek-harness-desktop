import { pathToFileURL } from 'node:url'

const entry = process.env.DSH_DESKTOP_ENTRY
const argumentsForDsh = process.argv.slice(1)
if (!entry) {
  throw new Error('DSH_DESKTOP_ENTRY is required')
}

const desktopPid = Number(process.env.DSH_DESKTOP_PARENT_PID)
if (Number.isInteger(desktopPid) && desktopPid > 0) {
  const parentWatch = setInterval(() => {
    try {
      process.kill(desktopPid, 0)
    } catch {
      process.exit(0)
    }
  }, 1_000)
  parentWatch.unref()
}

process.argv = [process.execPath, entry, ...argumentsForDsh]
await import(pathToFileURL(entry).href)

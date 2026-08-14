export const name = 'dsh-desktop-runtime'

export function apply(ctx, config = {}) {
  const desktopVersion = config.desktopVersion ?? 'unknown'
  const dshPackage = config.dshPackage ?? 'unknown'
  const desktopPid = Number(config.desktopPid)

  console.log(
    `[dsh-desktop] plugin loaded desktop=${desktopVersion} dsh=${dshPackage}`,
  )

  let parentWatch
  if (Number.isInteger(desktopPid) && desktopPid > 0) {
    parentWatch = setInterval(() => {
      try {
        process.kill(desktopPid, 0)
      } catch {
        process.exit(0)
      }
    }, 1_000)
    parentWatch.unref()
  }

  ctx.effect(() => () => {
    clearInterval(parentWatch)
    console.log('[dsh-desktop] plugin disposed')
  })
}

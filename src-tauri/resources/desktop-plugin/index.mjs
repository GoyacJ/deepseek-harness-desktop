export const name = 'dsh-desktop-runtime'

export function apply(ctx, config = {}) {
  const desktopVersion = config.desktopVersion ?? 'unknown'
  const dshPackage = config.dshPackage ?? 'unknown'

  console.log(
    `[dsh-desktop] plugin loaded desktop=${desktopVersion} dsh=${dshPackage}`,
  )

  ctx.effect(() => () => {
    console.log('[dsh-desktop] plugin disposed')
  })
}

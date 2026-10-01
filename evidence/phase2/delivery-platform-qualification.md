# Delivery platform qualification

Task `P2-DELIVERY-01`, qualification checkpoint based on
`paseo@5de45e208690b0efc51c59a585ae9729325a9204`.

## Platform state after qualification runs

| Platform | Observed evidence | Remaining gate |
|---|---|---|
| macOS | Unsigned disposable app install, launch, rejected update, upgrade, rollback, uninstall, and state retention | Signed artifact, notarization, Gatekeeper, network updater, `quitAndInstall` |
| Linux | Retained AppImage and real `dpkg` lifecycle, 8 direct tests plus 1 ignored real-dpkg test | Electron artifacts, updater network path, signing, RPM, ARM, restricted host |
| Windows | Locked all-target MSVC compile through the audio qualification runner | Native installer and updater runtime |
| Android | Historical AVD OS-command evidence only | APK install, update, rollback, uninstall, state retention |
| iOS | Command Line Tools only; iOS SDK and `simctl` unavailable | Signed or explicitly unsigned test package lifecycle on provisioned hardware |
| browser | No packaging or update lifecycle evidence | Frozen artifact install, update failure, rollback, state retention |

The bounded runner is `scripts/phase2/delivery-platform-qualification.sh`. Its
Linux mode verified the integrated evidence bytes without rebuilding or
rerunning the container. Its macOS mode passed the unsigned disposable app
lifecycle. The macOS log SHA-256 digest is
`a333a579255d584a3af1cd7ec70cf08ca6a148a29082da9926fcc3a0085d0c0f`.

## Signing and update boundary

No production key, signing identity, notarization service, store, deployment,
publication, or production update endpoint is used. Phase 2 accepts explicitly
unsigned test artifacts, but signed packaging and real updater delivery remain
open qualification work.

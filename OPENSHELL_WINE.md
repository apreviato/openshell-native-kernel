# OpenShell Native — Action plan for running Windows applications through Wine

**Version:** 1.0  
**Date:** November 12, 2026  
**Status:** actionable proposal; Wine on Asterinas **not validated**  
**Based on:** `OPENSHELL_NATIVE_ARCHITECTURE_FINAL.md` and *OpenShell Native — Migrating from Electron to CEF*  
**Initial target:** x86-64, Asterinas + Linux ABI, OpenShell shell in Rust/CEF, Debian apps through X11/Xpra, Wine in userspace.

> **Decision:** add Windows compatibility as an optional runtime managed by the same App Manager used for Linux applications. Keep Windows PE/Win32 out of the kernel, preserve the Debian graphics integration and leave the main frontend unchanged apart from adapters and specific UX.

## 1. Expected result

When a `.exe` is opened from Files or the App Launcher, OpenShell should detect the PE format, select Wine, prepare an isolated prefix, launch the process as an unprivileged user and present the window inside the OpenShell web desktop. The window takes part in the dock, focus, resize, minimise, workspaces and shutdown like a Debian application. Data and permissions survive the end of the session, without granting unrestricted access to the user's files.

The first delivery does **not** promise that every `.exe` will be compatible. PE/Win32 support depends at once on the Wine implementation, on the Debian userspace libraries and on the Linux semantics available on Asterinas.

### 1.1 Mandatory requirements

- Preserve the execution of Debian ELF, GTK/Qt and CLI applications, including the existing Xpra integration.
- Keep multi-process CEF and its sandbox separate from Wine and from untrusted executables.
- Use the same `Surface Registry` and the same logical window model for Linux and Windows apps.
- Do not add the Wine runtime to the desktop's default startup; start it on demand.
- Use a prefix and data directories per application; support import/export and backup.
- Record capabilities, logs, metrics and compatibility status per app.
- Keep the OpenShell Live/Debian fallback and the Native image rollback.

### 1.2 Out of scope at first

- Windows `.sys` drivers, kernel-level anti-cheat, NT kernel services and universal game compatibility.
- DXVK/VKD3D-Proton, DirectX 12, dedicated GPU support and NTSync optimisations without prior proof.
- Rewriting Wine, running Windows processes inside the CEF renderer or creating an NT kernel.
- Automatically running unknown installers with elevated privileges.

## 2. Target architecture

```mermaid
flowchart TD
  FE["OpenShell web UI — Files / Apps / Dock"] --> API["Host API — invoke / events"]
  API --> AM["Unified App Manager — Rust"]
  AM --> E{ "Application type" }
  E -->|Web| CEF["CEF app/browser context"]
  E -->|ELF| LIN["Debian App Runner"]
  E -->|PE32 / PE32+| WIN["Wine Runner"]
  WIN --> PM["Wine Prefix Manager"]
  WIN --> WI["Wine + wineserver + Win32 DLLs"]
  LIN --> GB["Linux Graphics Bridge — Xpra/X11 first"]
  WI --> GB
  GB --> SR["OpenShell Surface Registry"]
  CEF --> SR
  SR --> DISP["Display runtime / presentation"]
  WIN --> ABI["Asterinas Linux ABI + userspace"]
  LIN --> ABI
  DISP --> ABI
  ABI --> K["Asterinas Rust kernel"]
```

**Cohesion principle:** recognising PE and starting Wine are the App Manager's/runner's responsibilities; Win32 translation is Wine's; window integration is the graphics bridge's; window governance stays in the frontend through the Surface Registry. Do not create a second, Windows window manager.

### 2.1 Responsibilities and limits

| Component | Responsibility | Must not |
|---|---|---|
| `AppRegistry` | Registration, file associations, icons, commands, runtime type | Interpret DLLs/PE or run arbitrary commands |
| `AppManager` | Application lifecycle and identity, capabilities, choice of runner | Implement Wine logic internally |
| `WineRunner` | Resolve the runtime, start `wine`, track PID, wineserver, exit, logs | Control window layout |
| `WinePrefixManager` | Create, migrate, back up and clean the per-app prefix | Treat the prefix as a security sandbox |
| `LinuxGraphicsBridge` | Associate X11/Xpra windows and events with the application | Rewrite the application's widgets |
| `SurfaceRegistry` | Geometry, focus, z-order, visibility, workspace | Carry pixels through the JS API |
| `PermissionBroker` | Require consent and limit filesystem/network/devices | Rely on Wine's permissions alone |
| `Asterinas` | Linux ABI, processes, VFS, memory, signals, IPC and drivers | Expose Win32 as kernel syscalls |

## 3. Execution format and contract

### 3.1 Detecting executables

- Recognise the `MZ` magic, locate and validate the `PE\0\0` header, machine and sections without trusting the `.exe` extension.
- `PE32` and `PE32+` are Wine candidates; do not run a standalone DLL as an application without a proper entry point.
- Block PE files of unsupported architectures, malformed binaries and paths outside the granted capabilities.
- Do not treat the extension as sufficient evidence; show the origin, the signature when there is one and the risk of running it.
- For `.msi`, expose a separate flow through `msiexec`, only after the first installer phase.

### 3.2 Suggested contract

```rust
pub enum RuntimeKind {
    Web,
    Linux,
    Wine,
}

pub struct LaunchRequest {
    pub app_id: String,
    pub executable: std::path::PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: Option<std::path::PathBuf>,
    pub runtime: RuntimeKind,
    pub workspace_id: String,
}

pub struct LaunchResult {
    pub instance_id: String,
    pub process_id: Option<u32>,
    pub status: LaunchStatus,
}

pub enum LaunchStatus {
    Starting,
    Running,
    Failed,
}
```

The example defines the logical model, not an existing Rust API. In a real implementation, `LaunchRequest` should refer to identifiers/capabilities and not accept arbitrary paths supplied directly by the renderer; validation, quotas and authorisation must happen in the host.

### 3.3 Lifecycle events

`app.started`, `app.window-attached`, `app.focus-changed`, `app.exited`, `app.crashed`, `app.permission-required`, `wine.prefix-created`, `wine.prefix-migration-failed`, `wine.compatibility-warning`. All of them carry `app_id`, `instance_id`, generation and timestamp; window IDs are assigned by the host and bound to the process identity.

## 4. Code organisation

Adjust the directories after checking the real repositories; this tree is a proposed target, not a statement about the current files.

```text
openshell-native/
├── userspace/
│   ├── app-manager/
│   │   ├── registry/
│   │   ├── runners/
│   │   │   ├── linux/
│   │   │   └── wine/
│   │   └── permissions/
│   ├── wine/
│   │   ├── prefix-manager/
│   │   ├── runtime-manifest/
│   │   └── diagnostics/
│   ├── graphics/
│   │   ├── linux-graphics-bridge/
│   │   └── surface-registry/
│   └── host/
│       └── app-api/
├── distro/
│   └── packages/wine/
├── tests/
│   ├── wine/control-linux/
│   ├── wine/asterinas/
│   ├── wine/graphics/
│   └── wine/security/
└── docs/adr/
    └── wine-runtime.md

openshell/
└── apps/desktop/src/
    └── host/                  # UI adapter: Apps / Files / launch
```

## 5. Implementation phases and acceptance criteria

### Phase W0 — Inventory and baseline (blocking)

**Actions**
1. Pin the commit of Asterinas, patches, rootfs, CEF, Xpra and the Wine version; use stable Wine 11.0 as the initial baseline or another version explicitly pinned after comparison.
2. Inspect the existing Linux integration: how the App Manager launches ELF, how Xpra maps windows to the desktop, how focus/resize/close are propagated.
3. Define reproducible fixtures and observe `wine --version`, `ldd` dependencies, the display environment, filesystem and permissions.
4. Build the same Wine userspace image on the control Linux and in the Asterinas guest, without changing library versions between experiments.

**Delivery:** `compatibility-matrix.md`, version manifest, runners and baseline capture.  
**Gate W0:** Debian app execution remains intact; the Wine environment is reproducible on the control Linux.

### Phase W1 — Wine in userspace, without desktop integration

**Actions**
1. Package Wine, libraries, fonts, X11 dependencies and the required utilities in the rootfs; record each package's licence and checksum.
2. Create an isolated prefix in a persistent user directory and run `wineboot -i`, `winecfg` and `wine notepad.exe` under X11/Xpra.
3. Capture the first failure on Asterinas: syscall, flag, errno, ioctl, IPC, mmap, futex, signal or missing userspace dependency. Reproduce it with a minimal case.
4. Create control-Linux vs Asterinas tests and open an upstream issue only once the bug is located.
5. Do not disable the sandbox or run Wine as root to get around blockers.

**Diagnostic commands (lab only, example paths):**

```bash
export WINEPREFIX="$HOME/.local/share/openshell/wine/prefixes/smoke-notepad"
export WINEDEBUG=-all
wineboot -i
wine notepad.exe
wineserver -w
```

**Gate W1:** `wineboot` completes, Notepad opens and closes without orphan processes on the control Linux and on Asterinas; the logs identify the version and configuration. If the guest fails, the phase stays open with the gaps documented.

### Phase W2 — App Manager + `.exe` association

**Actions**
1. Add `RuntimeKind::Wine` to the app registry and PE detection to the host.
2. Create `WineRunner` as an App Manager adapter; the runner starts only on demand, with no mandatory global daemon.
3. Associate `.exe` with an **Open with Wine** action, explicit at first and subject to confirmation for untrusted executables.
4. Control the process group, PID, wineserver, cancellation, timeout, logs and cleanup. Do not mistake the closing of one window for the completion of a multi-process installer.
5. Expose `starting/running/failed/exited` states and understandable errors in the OpenShell UI.

**Gate W2:** double-clicking a supported `.exe` opens the application with its icon and identity in the App Manager; closing the app cleans up the expected processes without ending other applications.

### Phase W3 — Wine windows embedded in the desktop

**Actions**
1. Reuse the X11/Xpra path of Debian applications and bind Wine windows to the App Manager instance; ensure the binding through X11 metadata, PID/process tree and session tokens, without relying on titles alone.
2. Publish `surfaceId`, `appId`, `instanceId`, `generation`, rectangle, scale, focus, z-order and visibility in the `SurfaceRegistry`.
3. Test multiple windows of one process, modal dialogs, menus, the file picker, clipboard, drag and drop, IME, resize, minimise and workspaces.
4. Avoid carrying whole bitmaps through JS; the UI only arranges the window, while the bridge carries the surface.
5. Keep Xpra as the validated path; Wayland/XWayland is a later experiment, not an MVP dependency.

**Gate W3:** Notepad integrated with OpenShell's dock and window manager, with no orphan windows and without breaking desktop menus, Debian apps or the CEF Browser.

### Phase W4 — Prefix Manager, installation and persistent state

**Actions**
1. One prefix per application: `data/wine/prefixes/<app-id>`; persist the registry, drives and files. Keep cache and temporary files in a separate location.
2. Create an `.exe`/`.msi` installation wizard showing source, destination, prefix, requested components, progress and log; installation processes may open several windows.
3. Expose `winecfg`, the Wine version, dependencies, architecture and prefix export/import (with backups and version compatibility).
4. Map Windows drives only to explicitly allowed directories. A virtual C: folder is not the user's whole filesystem.
5. Register installed apps and shortcuts; do not blindly trust registry entries or executable names.
6. On A/B upgrades, migrate the prefix with a backup and a recorded version; allow rollback without corrupting the persistent profile.

**Gate W4:** install and restart a test application, keep its settings, uninstall it and restore a prefix backup; confirm that an image rollback does not damage the data.

### Phase W5 — Advanced compatibility and performance

**Actions**
1. Test 64-bit Win32 first, then 32-bit through WoW64, .NET applications and additional libraries, with separate fixtures.
2. Evaluate Wine Mono, fonts and VC runtimes as on-demand dependencies, with verified licences and sources.
3. Measure launch, RAM and latency in multi-process apps; separate the cost of Wine, Xpra, CEF and the kernel.
4. Evaluate the Wine Wayland driver and a more direct compositor once the guest really supports them; do not replace Xpra on a hypothesis.
5. Consider NTSync only if a real application shows a relevant bottleneck. Asterinas does not automatically inherit the Linux module; an equivalent interface would require specific development and validation.
6. DXVK/VKD3D and games depend on the evolution of Vulkan/GPU and on userspace support.

**Gate W5:** compatibility matrix published, regressions monitored and optimisations adopted only after a comparable benchmark.

## 6. Security and execution policies

**Threat model:** an executable `.exe` is untrusted code. Wine turns the app's calls into userspace operations and does not create isolation by itself. An isolated prefix prevents configuration conflicts; **it is not a sandbox**.

Mandatory controls:
- Run as an unprivileged user, never as root.
- Per-app/capability permissions, with consent for additional directories, network and clipboard integration where applicable.
- Restrict volume mounts and symlinks; prevent the `Z:` mapping from exposing the whole filesystem without consent.
- Additional isolation through namespaces, credentials and filters only where Asterinas really supports them and boundary tests exist; state explicitly when a protection is unavailable.
- Do not allow websites to call the `WineRunner` directly; access only through the authenticated and authorised App Manager.
- Process, memory, I/O and resource limits, when the kernel offers suitable mechanisms; a controlled fallback, not a pretence of isolation.
- Install dependencies from verified sources; logs must redact tokens, sensitive private directories and other credentials.
- Distinguish trust in a `.exe` from its signature: a signature does not guarantee safety.

**Launch block:** if there is no minimum isolation for an application's risk profile, OpenShell must refuse to run it or clearly communicate that the environment is experimental; never grant permissions silently.

## 7. Linux/Asterinas compatibility — diagnostic checklist

Check on the control Linux and on Asterinas (without assuming support):

| Area | Example tests | Symptom |
|---|---|---|
| Processes | clone/fork/exec, wait, signals, process groups | Wine does not start, or orphan processes |
| Memory | mmap/mprotect, TLS, shared memory | crash in the loader/DLL |
| Synchronisation | futex, waits, timers, eventfd | deadlock/high CPU |
| IPC | Unix sockets, pipes, wineserver | programs hang at startup |
| Filesystem | case sensitivity, symlinks, rename, locking, fsync | damaged prefix/installer |
| Desktop | X11 sockets, clipboard, IME, dialogs | invisible window or wrong input |
| Network | TCP/UDP, DNS, TLS, proxy | applications without connectivity |
| Audio | backend available in userspace | playback/recording unavailable |
| GPU | OpenGL, Vulkan, Mesa, DRM | software fallback / black screen |
| Isolation | supported user/pid/mount namespaces and filters | app without a security boundary |

Record the **first reproducible failure**, kernel/Wine version, host/guest, syscall, return value, trace and fixture. Make kernel changes only when the test shows the problem really is in the ABI and not in the rootfs, Wine or the graphics bridge.

## 8. Acceptance test matrix

| ID | Scenario | Criterion |
|---|---|---|
| WIN-001 | Initialise a prefix | `wineboot` without errors and without access outside the permissions |
| WIN-002 | Run Wine Notepad | Visible, working window; clean shutdown |
| WIN-003 | Open a `.exe` from Files | PE detected; Wine selected; identity in the dock |
| WIN-004 | Focus and resize | Correct input, z-index, scale and resize |
| WIN-005 | Dialogs and multiple windows | Children/modals bound to the original instance |
| WIN-006 | Multiple apps | Two isolated prefixes and independent processes |
| WIN-007 | Reopening the session | State preserved; no orphan processes |
| WIN-008 | Untrusted app | No extra permission without authorisation |
| WIN-009 | Debian apps | GTK/Qt keep working on the same desktop |
| WIN-010 | CEF Browser | Cookies and the page sandbox do not expose Wine/host |
| WIN-011 | Upgrade and rollback | Prefix intact when going back to the previous version |
| WIN-012 | Wine/Xpra failure | A diagnosable error without bringing down the OpenShell UI |
| WIN-013 | WoW64 | 32-bit test app under a compatible runtime |
| WIN-014 | Performance | Differences measured against the control Linux |

**Experimental release criterion:** WIN-001 to WIN-012, except WIN-011, which only becomes mandatory once A/B installation is enabled. WIN-013 and WIN-014 are gates for broadening compatibility, not for the first technical proof.

## 9. Observability and measurement

Each run should produce `run_id`, `app_id`, `instance_id`, Wine version, the `.exe` hash (without sharing the binary), kernel build ID, rootfs version, graphics backend, prefix ID, the times `request → wine spawned → first window → interactive`, RAM, CPU and exit code.

Priority metrics: cold/warm startup, extra RAM at rest, persistent processes, time to the first interactive window, input and resize latency, FPS when relevant, rendering failures, disk operations and stability across multiple sessions. Compare Wine on the same control Linux with and without the OpenShell integration; then compare with Asterinas using the same fixtures.

Do not set arbitrary numeric targets before measuring a baseline. Record p50/p95 and regressions per version.

## 10. Risks and mitigation

| Risk | Impact | Mitigation |
|---|---|---|
| Missing syscall/semantics in Asterinas | High | minimal repro, targeted patch, upstream |
| Wine starts but shows no window | High | validate X11/Xpra in isolation before the UI |
| Wine needs additional libraries | Medium | compatibility rootfs with pinned dependencies |
| A Windows executable reads private files | Critical | capabilities, limited user, proven isolation |
| Prefix corrupted after an update | High | backup, versioned migration and rollback |
| Poor composition performance | Medium/high | native surface, measure copies, avoid JS pixels |
| Multi-process installations | Medium | lifecycle by process tree and wineserver |
| Insufficient GPU/Vulkan | High for games | do not promise games in the MVP; software fallback |
| Wine updates break apps | Medium | versioned runtime and per-prefix compatibility |

## 11. Delivery plan and dependencies

| Priority | Delivery | Depends on | Result |
|---|---|---|---|
| P0 | W0 baseline | access to the current builds and smokes | reproducible environment |
| P0 | W1 `wineboot`/Notepad | X11/Xpra, Linux ABI | first Windows window |
| P1 | W2 PE launcher | App Manager / host API | open `.exe` from Files |
| P1 | W3 graphics integration | LinuxGraphicsBridge / Surface Registry | Windows window managed by OpenShell |
| P1 | Auditable minimum security | permissions / real isolation | controlled launch |
| P2 | W4 installers/prefixes | persistence and A/B | installable applications |
| P3 | W5 WoW64/.NET/DXVK | compatibility and drivers | broader ecosystem |

### Fallback policy

- Feature flag `native.wine.enabled=false` by default until WIN-001..012 are required for an experimental release.
- On a Wine error, keep the CEF/host session; show diagnostics and an option to try again.
- If the graphics backend fails, keep the Linux and Wine launchers disabled, without affecting the web applications.
- Do not revert a prefix automatically without a snapshot/backup; report an incompatible migration.
- Keep OpenShell Live/Debian as the reference and the way back.

## 12. Definition of Done for the first integrated milestone

- [ ] The same Wine version starts on the control Linux and on Asterinas.
- [ ] `wineboot` passes in a persistent isolated prefix.
- [ ] Wine's Notepad is rendered through X11/Xpra in the OpenShell window.
- [ ] Dock, focus, resize, minimise, maximise and close work.
- [ ] No orphan Wine processes remain after the application/session ends as expected.
- [ ] Opening a `.exe` from Files goes through the App Manager and asks for authorisation.
- [ ] Debian applications and the CEF Browser pass their smokes again.
- [ ] Private data is not implicitly exposed through Wine drives.
- [ ] Logs and reports record Wine/kernel/rootfs/CEF and reproducible failures.
- [ ] A release can disable Wine by configuration, without changing the Linux/web flow.

## 13. Proposed ADRs

- **WIN-ADR-01 — Optional Wine in userspace:** accepted as the direction; not included in the kernel.
- **WIN-ADR-02 — Unified App Manager:** extend the existing runner, with no parallel Windows manager.
- **WIN-ADR-03 — Xpra/X11 first:** adopted as the proof path; Wayland comes later.
- **WIN-ADR-04 — One prefix per app:** mandatory configuration isolation; it does not replace a sandbox.
- **WIN-ADR-05 — Versioned Wine:** a manifest per release, tested upgrades and data rollback.
- **WIN-ADR-06 — NTSync/DXVK conditional:** not part of the MVP without evidence of need and support.

## 14. Sources and verification notes

**Project documents provided:** `OPENSHELL_NATIVE_ARCHITECTURE_FINAL.md` (11/12/2026) and *OpenShell Native — Migrating from Electron to CEF* (11/11/2026). The second describes the Rust/CEF shell, X11/Openbox, `hostd`, the Browser overlay, Xpra and the smoke tests. This plan does not claim to have run or audited those tests.

**External references:**

- WineHQ, *Wine 11.0 Released* (01/13/2026): https://www.winehq.org/news/2026011301 — stable version, WoW64 and NTSync.
- WineHQ, news and releases: https://www.winehq.org/news/ — validate patches and versions before freezing a production runtime.
- Wine source/documentation: https://gitlab.winehq.org/wine/wine/-/wikis/Documentation
- Asterinas: https://asterinas.github.io/book/ — Linux compatibility and the limitations specific to the revision used.

**Condition for carrying out the plan:** validate the real state of the repository and the guest's tests before applying paths, patches, APIs or estimates. The proposed tree and the interfaces are implementation designs, not code already integrated.

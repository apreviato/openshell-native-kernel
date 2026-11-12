# OpenShell Native

Base experimental x86-64 do OpenShell com kernel em Rust e ABI Linux. O [plano de implementação](ARCHITECTURE.md) descreve a base do sistema. O [plano CEF](ARCHITECTURE-CEF.md) define a integração gráfica do Desktop Native.

## Boot e compatibilidade

Use Node.js 22+ e um engine Docker Linux amd64. No Windows, use Docker Desktop no modo Linux. O primeiro build baixa a imagem oficial do Asterinas, o código-fonte e as dependências de Rust e Nix. O QEMU usa TCG no contêiner, com discos virtuais descartáveis.

```text
node tools/build/native.mjs doctor
node tools/build/native.mjs build
node tools/build/native.mjs smoke
node tools/build/native.mjs control
node tools/build/native.mjs compat
node tools/build/native.mjs sandbox
node tools/build/native.mjs baseline
node tools/build/native.mjs graphics
node tools/build/native.mjs desktop <openshell-desktop-bundle>
```

| Comando | Resultado esperado |
| --- | --- |
| `doctor` | Verifica o engine e mostra a revisão e a versão de Rust esperadas. |
| `build` | Gera a ISO de diagnóstico upstream para QEMU UEFI com GRUB/Multiboot2 e initramfs. |
| `smoke` | Faz dois boots de controle upstream e um boot com sondas de PTY, ELF estático e I/O VirtIO ext2. |
| `control` | Executa as sondas de Node, Python e seccomp no Linux do builder com as versões fixadas pelo mesmo nixpkgs do guest. |
| `compat` | Inicia o initramfs com os runtimes e exige que as sondas de Node e Python passem. Os resultados de seccomp e de namespaces também aparecem no log. |
| `sandbox` | Exige, além das sondas de Node e Python, prova de que o kernel aplicou filtros seccomp e de que um processo sem privilégios entrou em novos namespaces de usuário, PID e rede. |
| `baseline` | Executa o controle Linux, os testes Rust do launcher, os três boots do smoke e o boot de compatibilidade. Registra os resultados de seccomp e de namespaces para decidir se os testes gráficos podem avançar. |
| `graphics` | Compila o shell Rust e o host CEF contra o CEF 147 do nixpkgs fixado e inicia um initramfs com Xvfb, Openbox, xdotool e o host. Exige, além das sondas do `compat`, as checagens de sandbox do Chromium, o servidor X e o [smoke do CEF](tools/desktop/cef-smoke.mjs) com o sandbox ativo. Usa 8 GB e 4 vCPUs no QEMU. |
| `desktop` | Inclui no initramfs o bundle OpenShell Desktop indicado e executa o [smoke do hostd](tools/desktop/hostd-smoke.mjs) como usuário sem privilégios, primeiro no Linux do builder e depois no guest, além das sondas do `compat`. |

Cada execução de build ou teste salva os artefatos em `out/native-*/`. `run.json` registra o resultado, inclusive falhas de conexão com o Docker, o digest do builder e os resultados de `checks.tsv` quando disponíveis. Quando há build de kernel, `toolchain.txt` registra firmware e perfil QEMU, e `SHA256SUMS` cobre a ISO de diagnóstico. Consulte `native-compat-console.txt` para diagnosticar falhas de ABI ou de sandbox no guest. `native-seccomp` e `native-namespaces` ficam como `unverified` quando o console não comprova que a sonda chegou a executar.

Se a sonda do Node falhar, o console registra os eventos recebidos pelo watcher em caso de timeout e os resultados de uma prova direta de `inotify` e `epoll` em `/ext2` e `/tmp`.

A origem, o commit, o Rust e o digest da imagem oficial ficam em [upstream.lock.json](kernel/config/upstream.lock.json). Para testar outro digest do builder, defina `OPENSHELL_NATIVE_BUILDER_DIGEST`. O `run.json` registra a imagem efetivamente usada.

O build aplica a [série de patches](kernel/patches/README.md) sobre a revisão fixada. O ajuste de build do OSDK usa os lockfiles dessa revisão ao compilar o OSDK e gerar a crate do kernel, o que evita que o nightly selecione `unwinding` 0.2.11. O `kernel-Cargo.lock` salvo no relatório mostra a resolução usada. Os demais patches adicionam filtros seccomp, os eventos de `rename` do inotify e os namespaces de usuário, PID e rede, exigidos pelo sandbox do Chromium e pelo hostd, e corrigem o que o Chromium e o hostd encontraram no caminho: portas efêmeras e PTYs para usuários comuns, PIDs locais no `wait4`, contagem de threads e arquivos de namespace no procfs, sockets Unix em `SCM_RIGHTS`, `MSG_DONTWAIT` e o título dos processos.

### Estado da compatibilidade

Resultados no perfil QEMU do baseline, com o kernel v0.19.0 e a série de patches. A coluna de controle indica a mesma sonda no Linux.

| Workload | Sonda | Linux de controle | Native |
| --- | --- | --- | --- |
| Boot upstream, PTY, ELF estático e I/O VirtIO ext2 | [boot.sh](userspace/compatibility/probes/boot.sh) | n/a | Passa |
| Python 3.13: processos, terminal, memória mapeada, `SCM_RIGHTS`, VFS, UDP, relógio e entropia | [python.py](userspace/compatibility/probes/python.py) | Passa | Passa |
| Node 24: subprocessos, Worker, `fs.watch` com `rename`, HTTP em loopback, DNS e TLS 1.3 contra fixtures locais | [node.mjs](userspace/compatibility/probes/node.mjs) | Passa | Passa com o patch 0003 |
| Filtros seccomp: `ERRNO`, `TRAP` com `SIGSYS`, `KILL`, modo estrito, `TSYNC`, herança em `fork` e `execve` | [seccomp.c](userspace/compatibility/probes/seccomp.c) | Passa | Passa com o patch 0002 |
| Namespaces de usuário, PID e rede sem privilégios, raiz vazia e descarte de capabilities, com casos negativos de isolamento | [namespaces.py](userspace/compatibility/probes/namespaces.py) | Passa | Passa com os patches 0004 a 0006 |
| hostd do OpenShell Desktop 2.1.0 como usuário comum: bootstrap e cookie, UI, autorização do WebSocket, RPC, `fs.watch` com rename, PTY real com resize e saída, `exec.run` com deadline | [hostd-smoke.mjs](tools/desktop/hostd-smoke.mjs) | Passa | Passa com os patches 0007 e 0008 |
| O que o Chromium verifica antes e durante o sandbox: links de `/proc/self/task`, arquivos de namespace, user namespace aninhado, zygote em namespaces com `exec` de `/proc/self/exe`, helper de `chroot` com `CLONE_VM \| CLONE_VFORK \| CLONE_SETTLS`, sockets em `SCM_RIGHTS`, dump netlink com `MSG_DONTWAIT` e título de processo | [chromium-userns.c](userspace/compatibility/probes/chromium-userns.c) | Passa | Passa com os patches 0009 a 0014 |
| CEF 147 em X11 (Xvfb e Openbox): janela, mouse, teclado, Web Worker, captura e renderers com seccomp aplicado pelo Chromium em namespaces próprios | [cef-smoke.mjs](tools/desktop/cef-smoke.mjs) modo `host` | Passa sem o sandbox[^control] | Passa com o sandbox |

[^control]: O builder roda em contêiner Docker, cujo perfil seccomp bloqueia user namespaces. O sandbox do Chromium foi comprovado no guest; no Linux de controle a bateria completa precisa de um host ou VM sem essa restrição.

As duas camadas do sandbox do Chromium, seccomp e namespaces, estão comprovadas nas sondas e no processo Chromium real: os renderers do CEF rodam no guest com `Seccomp: 2`, `NoNewPrivs: 1` e um filtro instalado pelo próprio Chromium. A prova usa Xvfb, sem aceleração. O guest não tem um driver GLX, e o backend gráfico efetivo do Chromium ainda não é registrado pelo smoke. Xorg sobre `/dev/dri/card0`, o registro do backend e o bundle Desktop real no CEF são as próximas etapas.

O builder restringe a criação de user namespaces, então a sonda de namespaces não entra no `control`. Para executá-la em Linux:

```bash
docker run --rm --security-opt seccomp=unconfined --security-opt apparmor=unconfined -v "$PWD/userspace/compatibility/probes:/probes:ro" python:3.13-slim python -I /probes/namespaces.py
```

A CI executa as sondas em Linux e aceita seleção de modo no workflow **Native baseline**. Para comparar com o OpenShell Live, registre a ISO amd64 e seu SHA-256 junto aos resultados.

## Shell experimental com CEF

O launcher Rust inicia o `hostd` offline e abre o bundle Desktop em uma janela CEF. A biblioteca Rust compartilhada valida a configuração entregue por stdin e decide quais origens podem navegar e carregar recursos. O adaptador C++ chama a biblioteca e implementa os callbacks de janela e ciclo de vida do SDK CEF.

Em um Linux x86-64, use um SDK CEF **Standard Distribution** já extraído, CMake, compilador C++ compatível com o SDK e Rust. Para executar os testes, inicie X11 com um gerenciador de janelas, use um usuário comum e disponibilize `xdotool`. O sandbox Chromium precisa estar disponível no kernel e na sessão.

```bash
bash tools/desktop/cef-baseline.sh /path/to/cef-sdk /opt/openshell
```

A bateria compila `cefsimple` e o host Native com o mesmo SDK e executa os testes de janela, entrada, Worker, isolamento e sandbox. Ela também executa um smoke separado com o `hostd` do bundle real. `out/cef-baseline-*/` contém a identificação do kernel e do CEF, os logs e resultados por teste e as capturas X11 quando `xwd` está disponível. Execute a bateria em Linux de controle e repita no guest Asterinas com a mesma distribuição CEF.

Para executar apenas a prova do bundle com Node.js 22+ em um host com loopback disponível:

```text
node tools/desktop/hostd-smoke.mjs /opt/openshell
```

O smoke usa um perfil descartável e testa a troca do token por cookie, o carregamento do HTML, a autenticação do WebSocket e os RPCs `system.info` e `fs.readdir`. Em seguida exercita os serviços de que Files, Memory, Terminal e as tools dependem: `fs.watch` com rename atômico, um PTY real com `stty`, resize e status de saída, e `exec.run` com saída, status e deadline. Ele registra o build e os hashes do `hostd` e do HTML, sem incluir o token de sessão, e grava o stderr do hostd em `hostd.log`.

## Integração com o OpenShell

O Native consome o bundle Linux do Desktop gerado no repositório `openshell` por `pnpm desktop:bundle`, em `apps/desktop/dist-bundle/openshell-desktop`. O bundle precisa conter `BUILD.json`, `hostd/openshell-hostd.mjs` e `ui/index.html`. O `run.json` da ação `desktop` registra a versão, a data de build, a versão do Electron e o SHA-256 do hostd.

```bash
node tools/build/native.mjs desktop ../openshell/apps/desktop/dist-bundle/openshell-desktop
```

O hostd usa o Node e o Python do mesmo nixpkgs do guest. O `node-pty` do bundle é opcional, e o hostd usa o backend `python-pty` quando ele falta. O guest ainda não tem sessão gráfica, então a UI é servida e verificada por HTTP, sem renderer.

Para compilar e abrir a UI real no launcher:

```bash
bash tools/desktop/build-cef.sh /path/to/cef-sdk
out/cef-build/rust/release/openshell-shell /opt/openshell \
  out/cef-build/host/runtime/openshell-cef-host "$HOME/.local/state/openshell-native"
```

O bundle em `/opt/openshell` precisa conter `hostd/openshell-hostd.mjs` e `ui/index.html`. O launcher usa o Node incluído no bundle ou o Node disponível no PATH. Ele usa a porta local `7788` por padrão. Defina `OPENSHELL_NATIVE_PORT` antes do primeiro boot de um perfil se precisar de outra porta.

`node tools/desktop/cef-smoke.mjs <sample|host|isolation> <cef-executable>` executa uma prova gráfica individual. Os resultados ficam em `out/cef-*/cef-smoke.json`.

O modo `isolation` abre duas views na mesma janela X11, com contextos independentes e dois servidores de teste locais. O teste exige entrada em ambas as views antes e depois de redimensionar a janela. Também verifica cookies e bloqueio de requisições diretas, redirecionadas e iniciadas por Worker. A prova usa views lado a lado. A composição das abas nas janelas da UI e a ponte de capacidades ficam na próxima etapa.


## Desktop de referência com Electron em X11

Em um guest Linux com Xorg já ativo, execute com um usuário comum. Passe a pasta do bundle do OpenShell Desktop e o executável Electron Linux correspondente ao `BUILD.json` do bundle.

```text
node tools/desktop/smoke.mjs /opt/openshell /opt/openshell/electron/electron
```

O teste usa perfil descartável, hostd offline e uma página local com Web Worker no Browser. O relatório exige o preload do Desktop, a ponte hostd, o resultado do worker e evidência de filtros seccomp nos processos renderer. Para diagnosticar falhas, consulte `native-desktop.json`, o relatório do Electron e o log em `out/desktop-*/`.

A revisão fixada do Asterinas omite a syscall `seccomp` na tabela x86-64, e o patch 0002 a adiciona junto com os campos de `/proc/[pid]/status` que este teste lê. O teste `sandbox` exige que o guest instale e aplique um filtro e que entre em novos namespaces. O resultado dessa prova condiciona a integração do kiosk. [Tabela de syscalls da revisão fixada](https://github.com/asterinas/asterinas/blob/c4b7ed575b7b32773475b194d2ad41fb6d03765b/kernel/core/src/syscall/arch/x86.rs)

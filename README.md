# OpenShell Native

Base experimental x86-64 do OpenShell com kernel em Rust e ABI Linux. O [plano de implementação](ARCHITECTURE.md) descreve a base do sistema. O [plano CEF](ARCHITECTURE-CEF.md) define a integração gráfica do Desktop Native.

## Boot e compatibilidade

Use Node.js 22+ e um engine Docker Linux amd64. No Windows, use Docker Desktop no modo Linux. O primeiro build baixa a imagem oficial do Asterinas, o código-fonte e as dependências de Rust e Nix. O QEMU usa TCG no contêiner, com discos virtuais descartáveis.

### Gerar a mídia

```bash
make bundle
make iso
make install-image
```

```powershell
.\make-live.ps1
.\make-live.ps1 -BuildOnly
.\make-live.ps1 -BuildOnly -InstallImage
```

No Windows, execute `make-live.ps1` como administrador para criar um pendrive. O script lista os dispositivos USB elegíveis, pede confirmação do número e só então começa o build. Confere a identidade novamente antes de apagar o dispositivo e compara o SHA-256 lido do pendrive com o da imagem. Use `-Iso <arquivo>` para gravar uma ISO existente com seu `.sha256`, `-DiskNumber <número>` para indicar o dispositivo diretamente e `-WhatIf` para ver o plano.

Se o bundle já existe, use `BUNDLE=/caminho/do/bundle` no Make ou `-Bundle <diretório>` no PowerShell. Cada execução produz a mídia, o arquivo `.sha256` e o relatório `run.json` em `out/native-*/`. `make iso` gera uma ISO live x86-64 para UEFI com estado temporário. `make install-image` gera um disco GPT de referência com slots A/B, recovery e dados persistentes. As duas mídias incluem o Desktop, Xpra e Wine 11.0. O PowerShell verifica o SHA-256 depois de copiar a mídia para o host.

#### Secure Boot

As mídias iniciam como as do OpenShell Live: o firmware carrega o shim do Debian, assinado pela Microsoft, e o shim carrega o GRUB assinado pelo Debian, das versões fixadas em [secureboot.lock](distro/image/secureboot.lock). O Asterinas não pode ter a assinatura do Debian. Por isso, o kernel é assinado com a chave do OpenShell Native, e o GRUB pede ao shim que o confira contra as chaves do dono da máquina (MOK). Com Secure Boot ligado, cada computador inscreve o certificado uma vez:

1. No menu do pendrive, escolha **Enroll the OpenShell Native key**. O MokManager do Debian abre.
2. Escolha **Enroll key from disk**, o volume do pendrive, `ENROLL_THIS_KEY_IN_MOKMANAGER.cer`, **Continue** e **Yes**, e reinicie.

Sem o certificado, o GRUB recusa o kernel ("bad shim signature"). Nesse caso, **Try OpenShell Native** mostra os passos acima e abre o MokManager sozinho, e o disco instalado, quando não inicia nenhum slot, faz o mesmo. O Live não pede isso porque inicia o kernel Linux do próprio Debian, assinado com a chave do Debian que o shim já traz. O shim só aceita, além dessa chave, as do MOK, e o Debian não assina outros kernels. Dispensar a inscrição exigiria um shim do OpenShell com a sua chave embutida e assinado pela Microsoft (o processo de revisão do shim, feito por quem publica a distribuição), ou desligar o Secure Boot. Com Secure Boot desligado, nada é pedido. O GRUB assinado do Debian não tem `multiboot2` sob Secure Boot. Por isso, o kernel é construído com `BOOT_PROTOCOL=linux-efi-handover64` e entra pelo `linux`/`initrd` do GRUB, inclusive nas validações em QEMU.

A chave nasce no primeiro build e fica em `.secureboot/` (fora do Git); os builds seguintes a reutilizam, para que o certificado inscrito continue valendo. Guarde uma cópia: mídias assinadas com outra chave pedem nova inscrição. `OPENSHELL_SECURE_BOOT_KEYS` aponta outra pasta. O teste do disco ([boot-test.sh](distro/image/boot-test.sh)) inicia o slot com o OVMF em Secure Boot e as chaves da Microsoft: primeiro sem o certificado, com o kernel recusado, e depois com ele.

#### Carregamento

O boot mostra a mesma tela de carregamento do OpenShell Live: o wordmark OPENSHELL e a barra sobre o quase-preto do Desktop. O GRUB desenha [splash.png](distro/image/splash.png) (feito por [make-splash.py](distro/image/make-splash.py) com as imagens do tema Plymouth do Live, na mesma geometria) enquanto carrega o kernel e o initramfs, e o [splash.cfg](distro/image/splash.cfg) da ESP serve ao menu do pendrive e ao disco instalado. O GRUB entrega a tela como está (`gfxpayload=keep`), o stub EFI não escreve nela (patch [0022](kernel/patches/README.md)) e o kernel não a apaga (patch 0023). A tela de carregamento do próprio Desktop, com o mesmo desenho, assume em seguida, e depois vem a entrada. As mensagens do GRUB continuam na porta serial, que os testes leem.

#### Desempenho da tela

O Native não tem driver de GPU: a tela é o framebuffer do firmware, pelo simpledrm. Tudo é desenhado no processador, e cada camada evita trabalho que só faria sentido com GPU:

- O Chromium compõe em software (`--disable-gpu-compositing`), em vez de emular uma GPU com o SwiftShader ou o llvmpipe a cada quadro. O WebGL continua no SwiftShader.
- O Desktop se declara sem GPU, como o Live faz nessas máquinas, e deixa de fora os desfoques de fundo, que seriam recalculados a cada quadro sob o dock e os menus.
- O kernel copia para a tela só as áreas que o X informa como alteradas (patch [0025](kernel/patches/README.md)). Antes, cada movimento do ponteiro copiava a tela inteira.
- O X recebe as imagens dos apps por memória compartilhada (MIT-SHM), que o kernel passou a oferecer com a memória compartilhada SysV (patch 0026). Antes, o Chromium, o Xpra e os apps GTK mandavam cada imagem pelo socket do X.

#### Tela, teclado e mouse

A sessão das mídias mostra o Desktop na tela do computador. O [openshell-x](userspace/compatibility/probes/openshell-x.sh) inicia o Xorg com o driver `modesetting` sobre o `/dev/dri/card0` do kernel (simpledrm sobre o framebuffer do UEFI, na resolução que o firmware entregou), com o driver `evdev` em cada `/dev/input/event*`. Sem udev nem logind, o script entrega a tela, as entradas e o console ao usuário da sessão antes de iniciá-la, como o logind faz com o seat ativo, e o X roda sem privilégios. Sem DRM, ele inicia o Xvfb, como nos smokes em QEMU. No QEMU com framebuffer UEFI, o X desenha na tela do firmware (captura por `screendump`), e teclas e o mouse PS/2 chegam ao X. O teclado depende do patch [0019](kernel/patches/README.md). Teclados e mouses USB de qualquer marca funcionam, ligados antes ou depois do boot: o kernel dirige os controladores xHCI e a classe HID (patch [0031](kernel/patches/README.md)), com roda, roda horizontal, botões extras e teclas de mídia; como o X não ganha dispositivos depois de iniciar, todos entram por `usb_keyboard` e `usb_mouse`. Hubs, pendrives e outros dispositivos USB ainda não funcionam. O touchpad tem os gestos que o libinput dá ao Live: o kernel liga o modo absoluto dos touchpads Synaptics e ELAN (versões 3 e 4) e transforma os dedos em ponteiro, toque para clicar (um dedo: esquerdo, dois: direito, três: meio), rolagem natural com dois dedos, o canto inferior direito do clickpad como botão direito e nenhum toque que começa enquanto se digita (patch [0028](kernel/patches/README.md)). O X recebe um mouse com rodas, porque sem udev o libinput não funciona. Outros touchpads (ALPS, ELAN antigos) continuam como mouse PS/2, e o registro do kernel diz qual foi encontrado. Neles, e em qualquer mouse, segurar o botão direito e mover também rola. Um mouse PS/2 com roda rola com ela. Configurações ▸ Cursor muda a cor e o tamanho do ponteiro, como no Live: a sessão inicia o ajudante do cursor do Desktop antes dele, e os programas usam o tema "OpenShell". Não há aceleração 3D; a tela usa o framebuffer do firmware, sem troca de resolução nem segundo monitor.

#### Entrada

O sistema instalado pede a senha antes do Desktop. O supervisor `openshell-session`, como root, inicia o X e o Openbox e depois o Desktop como greeter. É o mesmo bundle, na página de entrada, como o usuário da sessão. O shell só começa depois da senha certa. A senha é escolhida no instalador, e não há como recuperá-la; um sistema sem senha (instalado por uma versão anterior) a cria no primeiro boot, na página de entrada. O hash SHA-512 crypt (`$6$`, do `cryptpw` do BusyBox, que lê a senha de um descritor e nunca dos argumentos) fica em `/data/openshell/account/password`, de root, modo 0600. A página pergunta pelo hostd, que repassa a um socket Unix do supervisor. O supervisor só responde a processos do usuário da sessão (`SO_PEERCRED`) e espera 2 s antes de cada resposta a uma senha errada. A sessão live entra sem senha, como a do OpenShell Live. O [smoke da sessão](tools/desktop/session-smoke.mjs) exige que o shell só comece depois da entrada, e o teste do disco ([image-boot.sh](userspace/compatibility/probes/image-boot.sh)) entra no Desktop instalado pelo mesmo socket.

#### Bloqueio de tela e energia

O bloqueio é o do OpenShell Live, com a mesma tela: o XSecureLock cobre a tela e segura teclado e mouse, e a tela de bloqueio do Live é o módulo de autenticação dele ([lock/](userspace/compatibility/lock/README.md)). A senha é a da entrada: o `openshell-authproto` a confere com o supervisor pelo socket do sistema, com a mesma espera depois de uma senha errada. O botão Bloquear e o tempo sem uso de Configurações ▸ Tela de bloqueio bloqueiam a tela; no Native, o Desktop informa ao hostd o tempo sem uso que o X registra, porque não há logind para o `xss-lock`. A sessão live, sem senha, não bloqueia, como a do Live. A tela de bloqueio guarda a senha só em memória travada, e o kernel aceita `mlock` (patch 0024).

Desligar e reiniciar, no Desktop e na tela de bloqueio, também passam pelo supervisor, que não tem `CAP_SYS_BOOT`: ele encerra a sessão e sai com o código da ação, e o init grava o disco de dados (`sync`, desmontagem) e desliga ou reinicia. Não há suspensão.

#### Instalação

O pendrive live instala o sistema, como o Calamares no Live: **Install OpenShell** no Desktop live lista os discos internos (NVMe e VirtIO, de 16 GiB ou mais), pede confirmação (**Erase and install**), mostra o progresso e oferece reiniciar no fim. O supervisor roda o [instalador](distro/image/install.sh) como root. Ele cria a tabela GPT do disco de referência (ESP, `system_a`, `system_b`, recovery e dados), grava a cadeia assinada e o splash na ESP, a versão no slot A, já confirmada, e a mesma versão como recovery. Antes, como o Calamares, ele pergunta o nome, a senha (duas vezes), o teclado e o fuso horário; o supervisor confere cada um e guarda só o hash da senha, e o instalador grava tudo para o sistema instalado, que já abre na tela de bloqueio. O kernel não lê o pendrive depois do boot (não há pilha USB), então o sistema live roda inteiro da memória, e o instalador copia o initramfs que o computador carregou, exposto em `/dev/initrd` (patch [0021](kernel/patches/README.md)). O sistema live precisa de memória para o sistema descompactado além do uso normal; o sistema instalado, só no primeiro boot de cada versão ([Memória](#memória)). Por isso, a mídia deixa de fora o Mesa (o CEF usa o SwiftShader dele para WebGL) e as aplicações de teste. O [teste do instalador](distro/image/install-test.sh) instala em um NVMe do QEMU e inicia o disco sozinho pela cadeia assinada.

A revisão atual do kernel oferece VirtIO e NVMe. Boot de um sistema instalado em SATA/AHCI, armazenamento USB e outros controladores depende de suporte no kernel.

#### Memória

O sistema instalado não fica mais inteiro na memória. Antes, cada boot descompactava o initramfs da versão (2,8 GB, com o Wine, o CEF, o Python e o Node) na RAM e ainda guardava o compactado (1 GB), quase 4 GB antes do Desktop. Agora o primeiro boot de um slot depois da instalação ou de uma atualização ainda roda da memória e, em segundo plano e com prioridade baixa, copia o sistema do `/dev/initrd` para `/data/openshell/system-root/<slot>` ([installed-session.sh](userspace/compatibility/probes/installed-session.sh)). Dali em diante, o `boot.cfg` do slot ([release.sh](distro/image/release.sh)) carrega um initramfs de poucos megabytes ([disk-root.sh](userspace/compatibility/probes/disk-root.sh), com o BusyBox da imagem): ele monta a partição de dados, faz da cópia a raiz e põe a própria partição em `/data` (a mesma montagem, nunca uma segunda do mesmo disco) e `/tmp` na memória. O kernel lê do disco o que é usado e guarda em cache, como o squashfs do Live.

O bloco de ambiente do GRUB de cada cópia (`<slot>.env`) diz para qual versão ela está boa. O GRUB marca a tentativa antes de entregar o controle; o sistema marca a cópia como boa depois de 90 s rodando, quando o Desktop sobe ou num desligamento limpo. Uma tentativa que não chegou lá, uma cópia de outra versão ou nenhuma faz o GRUB iniciar da memória como antes, e esse boot refaz a cópia; depois de duas falhas, o slot fica na memória até a próxima atualização. Uma atualização invalida a cópia do slot que ela grava. A cópia precisa de 6 GiB livres na partição de dados (a do outro slot sai primeiro, se faltar espaço). No QEMU, o boot pela cópia sobe com a raiz em `nvme0n1p5`, a mesma partição em `/data` e quase toda a RAM livre, e os três boots (cópia boa, tentativa refeita, tentativa sem marca) seguem o caminho esperado. O sistema live e o recovery continuam na memória.

#### Configurações do sistema

O Live muda o fuso horário com `timedatectl` e o nome da conta com `sudo chfn`. O Native não tem nenhum dos dois, e sua raiz vive na memória. Em Configurações, o Desktop pede essas mudanças ao supervisor pelo socket do sistema (`timezone <Área/Cidade>`, `name <nome>`, [settings.rs](userspace/shell/src/settings.rs)). O supervisor aponta `/etc/localtime` para a zona do `tzdata` e troca o nome da conta no `/etc/passwd`. No sistema instalado, ele guarda os dois em `/data/openshell/system` e os aplica de novo a cada boot, antes da sessão. O relógio é o do firmware: não há hora pela rede nem ajuste manual do horário. Configurações ▸ Tela usa o `xrandr` e mostra a única saída do simpledrm. O brilho, nas Configurações e nas teclas, é do backlight da tela embutida (`/sys/class/backlight`, patch 0030), que só root escreve: o Desktop o pede ao supervisor (`brightness <1-100>`), que o grava e o reaplica a cada boot, como o systemd-backlight do Live.

#### Logs, processos e desempenho

O Native não tem journald. Configurações ▸ Sistema ▸ Logs mostra, em **Sistema**, os registros do kernel (`/dev/kmsg`, que o kernel passou a guardar mesmo sem console serial, patch [0029](kernel/patches/README.md)) e os logs deste boot que o supervisor guarda em `session-logs/NNNN` (o diário dele, o X e o Openbox); em **Minha sessão**, o Desktop e o hostd (`apps/desktop/hostd/native/logs.ts` no repositório do OpenShell). O supervisor grava os logs como root, e o grupo da sessão os lê. A aba Serviços diz que o Native não tem serviços do systemd. Processos lê o `/proc` quando o `ps` não tem `-o` nem `--sort` (o do BusyBox), com o mesmo cálculo de CPU e memória do `ps`. Desempenho usa `/proc/stat`, `/proc/meminfo` e `/proc/loadavg`, que o kernel oferece; rede e disco ficam em zero, porque faltam `/proc/net/dev` e `/proc/diskstats`, e não há sensores de temperatura.

#### Paridade com o OpenShell Live

| Recurso do Live | No Native |
| --- | --- |
| Boot pelo shim/GRUB do Debian com Secure Boot | Igual; o certificado do kernel é inscrito uma vez no MokManager (ver [Secure Boot](#secure-boot)). |
| Tela de carregamento (Plymouth) | Igual, desenhada pelo GRUB e mantida até o Desktop ([Carregamento](#carregamento)). |
| Entrada com senha, bloqueio de tela, desligar e reiniciar | Iguais ([Entrada](#entrada), [Bloqueio de tela e energia](#bloqueio-de-tela-e-energia)); sem suspensão. |
| Instalação pelo Calamares | Instalador do Desktop live, com o disco A/B ([Instalação](#instalação)). |
| Desktop, Terminal, Files, Browser, apps Linux (Xpra) e Windows (Wine) | Os mesmos, sobre o CEF ([Shell com CEF](#shell-com-cef)). |
| Fuso horário, nome da conta, teclado, tela | Pelo supervisor e pelas ferramentas X ([Configurações do sistema](#configurações-do-sistema)); nome, senha, teclado e fuso também no instalador. |
| Touchpad (libinput: toque, rolagem natural, clickpad, digitação) | Synaptics e ELAN v3/v4 pelo kernel; outros como mouse ([Tela, teclado e mouse](#tela-teclado-e-mouse)). |
| Cor e tamanho do cursor | Iguais, pelo mesmo ajudante do Desktop. |
| Teclado inteiro, Num Lock lembrado, Print Screen | Iguais (patch 0027, numlock.py). |
| Brilho (teclas e Configurações) | Telas de notebooks Intel, pelo PWM do backlight (patch 0030), guardado entre boots; AMD ainda não. |
| Teclado e mouse USB | Iguais, de qualquer marca, também ligados depois do boot (patch 0031); sem hubs. |
| Teclas de volume | Chegam ao Desktop, mas o kernel ainda não tem som. |
| Logs, processos, desempenho | Do kernel e do supervisor, sem journald ([Logs, processos e desempenho](#logs-processos-e-desempenho)). |
| Memória (raiz em squashfs) | O sistema instalado inicia da cópia na partição de dados depois do primeiro boot de cada versão ([Memória](#memória)). |
| Rede cabeada e Wi-Fi, som, Bluetooth, pendrives e discos USB, impressoras, firewall | Dependem de drivers e de partes do kernel que ainda faltam (rede só VirtIO com endereço fixo e sem DHCP, sem armazenamento USB nem FAT, sem áudio, sem netfilter). As páginas mostram o recurso como indisponível. |
| Atualizações do sistema | Configurações ▸ Atualizações, pelo canal de releases do Native, gravadas no outro slot A/B ao reiniciar ([Canal de releases](#canal-de-releases)). Não há "restaurar": uma versão que não inicia volta sozinha à anterior. |

### Sondas e builds de desenvolvimento

```text
node tools/build/native.mjs doctor
node tools/build/native.mjs build
node tools/build/native.mjs smoke
node tools/build/native.mjs control
node tools/build/native.mjs compat
node tools/build/native.mjs sandbox
node tools/build/native.mjs baseline
node tools/build/native.mjs image
node tools/build/native.mjs desktop-image <openshell-desktop-bundle>
node tools/build/native.mjs graphics
node tools/build/native.mjs desktop <openshell-desktop-bundle>
node tools/build/native.mjs kiosk <openshell-desktop-bundle>
node tools/build/native.mjs apps <openshell-desktop-bundle>
node tools/build/native.mjs wine <openshell-desktop-bundle>
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
| `image` | Testa discos GPT descartáveis com OVMF e GRUB. Verifica a atualização do slot inativo pelo Asterinas, três tentativas sem confirmação e retorno, confirmação do novo slot e recuperação com metadados ou configurações de boot ausentes. Os logs por boot ficam em `out/native-*/native-image/`. |
| `desktop-image` | Inicia o bundle do Desktop a partir do slot em teste e usa a sessão instalada em `data` para confirmar o boot. Um segundo boot verifica a confirmação persistida. |
| `graphics` | Compila o shell Rust e o host CEF contra o CEF 147 do nixpkgs fixado e inicia um initramfs com Xvfb, Mesa, Openbox, xdotool e o host. Exige, além das sondas do `compat`, as checagens de sandbox do Chromium, o servidor X e o [smoke do CEF](tools/desktop/cef-smoke.mjs) com o sandbox ativo, nos modos `host` e `isolation`. Os mesmos smokes rodam antes no Linux do builder, com o sandbox. Usa 12 GB e 4 vCPUs no QEMU. |
| `desktop` | Inclui no initramfs o bundle OpenShell Desktop indicado e executa o [smoke do hostd](tools/desktop/hostd-smoke.mjs) como usuário sem privilégios, primeiro no Linux do builder e depois no guest, além das sondas do `compat`. |
| `kiosk` | Reúne `graphics` e `desktop` e abre o bundle indicado no shell. O [smoke do shell](tools/desktop/shell-smoke.mjs) dirige o Desktop real com entrada X11: Files, Terminal, atalhos e foco do Browser nativo, zoom, download, DevTools, seleção de arquivos, PDF, captura de tela com as abas, histórico do clipboard, recuperação do renderer e do hostd, encerramento e segunda sessão. O [smoke da sessão](tools/desktop/session-smoke.mjs) executa o supervisor `openshell-session` como root. Os dois rodam antes no Linux do builder. |
| `apps` | A `kiosk` com o perfil de apps Linux: Xpra, seu cliente HTML5, D-Bus e um app de prova GTK e um Qt. O [smoke de apps](tools/desktop/apps-smoke.mjs) abre os apps pela API do `hostd` e exige o encerramento sem órfãos. O smoke do shell acrescenta a fase `linux-apps`: os apps abrem pela busca em janelas do OpenShell, recebem teclado e o tamanho da janela, mostram diálogos, trocam texto com o histórico do clipboard, abrem documentos pelo Terminal e pelo Files e fecham. Usa 12 GB e 4 vCPUs no QEMU, com até 120 minutos de boot. |
| `wine` | A `apps` com o perfil de apps Windows ([plano](OPENSHELL_WINE.md)): Wine 11.0, de 64 bits com WoW64, e uma entrada que abre `.exe` e `.msi` em primeiro plano. O [smoke do Wine](tools/desktop/wine-smoke.mjs) cria um prefixo isolado com `wineboot`, abre e fecha o Notepad em X11 e exige que nada do prefixo fique para trás. O smoke do shell abre um `.exe` pelo Terminal, escolhe o Wine e exige a janela do programa no Desktop e o fim do Wine ao fechá-la. Usa 12 GB e 4 vCPUs no QEMU, com até 150 minutos de boot. |

Cada execução de build ou teste salva os artefatos em `out/native-*/`. `run.json` registra o resultado, inclusive falhas de conexão com o Docker, o digest do builder e os resultados de `checks.tsv` quando disponíveis. Quando há build de kernel, `toolchain.txt` registra firmware e perfil QEMU, e `SHA256SUMS` cobre a ISO de diagnóstico. Consulte `native-compat-console.txt` para diagnosticar falhas de ABI ou de sandbox no guest. `native-seccomp` e `native-namespaces` ficam como `unverified` quando o console não comprova que a sonda chegou a executar.

Se a sonda do Node falhar, o console registra os eventos recebidos pelo watcher em caso de timeout e os resultados de uma prova direta de `inotify` e `epoll` em `/ext2` e `/tmp`.

A origem, o commit, o Rust e o digest da imagem oficial ficam em [upstream.lock.json](kernel/config/upstream.lock.json). Para testar outro digest do builder, defina `OPENSHELL_NATIVE_BUILDER_DIGEST`. O `run.json` registra a imagem efetivamente usada.

O build aplica a [série de patches](kernel/patches/README.md) sobre a revisão fixada. O ajuste de build do OSDK usa os lockfiles dessa revisão ao compilar o OSDK e gerar a crate do kernel, o que evita que o nightly selecione `unwinding` 0.2.11. O `kernel-Cargo.lock` salvo no relatório mostra a resolução usada. Os demais patches adicionam filtros seccomp, os eventos de `rename` do inotify e os namespaces de usuário, PID e rede, exigidos pelo sandbox do Chromium e pelo hostd, e corrigem o que o Chromium e o hostd encontraram no caminho: portas efêmeras e PTYs para usuários comuns, PIDs locais no `wait4`, contagem de threads e arquivos de namespace no procfs, sockets Unix em `SCM_RIGHTS`, `MSG_DONTWAIT`, o título dos processos e o `bind` de um servidor reiniciado na mesma porta.

### Estado da compatibilidade

Resultados no perfil QEMU do baseline, com o kernel v0.19.0 e a série de patches. A coluna de controle indica a mesma sonda no Linux.

| Workload | Sonda | Linux de controle | Native |
| --- | --- | --- | --- |
| Boot upstream, PTY, ELF estático e I/O VirtIO ext2 | [boot.sh](userspace/compatibility/probes/boot.sh) | n/a | Passa |
| Python 3.13: processos, terminal, memória mapeada, `SCM_RIGHTS`, VFS, UDP, relógio e entropia | [python.py](userspace/compatibility/probes/python.py) | Passa | Passa |
| Servidor TCP reiniciado na mesma porta enquanto as conexões anteriores fecham | `python.py`, sonda `server_restart` | Passa | Passa com o patch 0015 |
| Transação A/B no disco VirtIO: atualização confirmada, teste de 3 boots sem confirmação e retorno, release adulterado, schema incompatível e seletor destruído | [smoke.sh](distro/ab/smoke.sh) com o `openshell-ab` | Passa | Passa em `/ext2` |
| Node 24: subprocessos, Worker, `fs.watch` com `rename`, HTTP em loopback, DNS e TLS 1.3 contra fixtures locais | [node.mjs](userspace/compatibility/probes/node.mjs) | Passa | Passa com o patch 0003 |
| Filtros seccomp: `ERRNO`, `TRAP` com `SIGSYS`, `KILL`, modo estrito, `TSYNC`, herança em `fork` e `execve` | [seccomp.c](userspace/compatibility/probes/seccomp.c) | Passa | Passa com o patch 0002 |
| Namespaces de usuário, PID e rede sem privilégios, raiz vazia e descarte de capabilities, com casos negativos de isolamento | [namespaces.py](userspace/compatibility/probes/namespaces.py) | Passa | Passa com os patches 0004 a 0006 |
| hostd do OpenShell Desktop 2.1.0 como usuário comum: bootstrap e cookie, UI, autorização do WebSocket, RPC, `fs.watch` com rename, PTY real com resize e saída, `exec.run` com deadline | [hostd-smoke.mjs](tools/desktop/hostd-smoke.mjs) | Passa | Passa com os patches 0007 e 0008 |
| O que o Chromium verifica antes e durante o sandbox: links de `/proc/self/task`, arquivos de namespace, user namespace aninhado, zygote em namespaces com `exec` de `/proc/self/exe`, helper de `chroot` com `CLONE_VM \| CLONE_VFORK \| CLONE_SETTLS`, sockets em `SCM_RIGHTS`, dump netlink com `MSG_DONTWAIT` e título de processo | [chromium-userns.c](userspace/compatibility/probes/chromium-userns.c) | Passa | Passa com os patches 0009 a 0014 |
| CEF 147 em X11 (Xvfb e Openbox): janela, mouse, teclado, Web Worker, WebGL, captura e renderers com seccomp aplicado pelo Chromium em namespaces próprios | [cef-smoke.mjs](tools/desktop/cef-smoke.mjs) modo `host` | Passa com o sandbox[^control] | Passa com o sandbox |
| Dois contextos CEF lado a lado: cookies separados e requisições diretas, redirecionadas e de Worker bloqueadas, antes e depois de redimensionar | `cef-smoke.mjs` modo `isolation` | Passa com o sandbox | Passa com o sandbox |
| OpenShell Desktop real no shell: Files, Terminal, Browser com aba nativa, recuperação do renderer e do hostd, segunda sessão e segredo fora dos logs | [shell-smoke.mjs](tools/desktop/shell-smoke.mjs) | Passa com o sandbox | Passa com o sandbox e os patches 0001 a 0015 |
| Sessão supervisionada: X, Openbox e shell como usuário comum, reinício do shell sem processos remanescentes e encerramento ordenado | [session-smoke.mjs](tools/desktop/session-smoke.mjs) | Passa | Passa |

[^control]: O builder das ações `graphics`, `kiosk` e `apps` roda sem o perfil seccomp do Docker, que bloqueia user namespaces. Em CI, o runner precisa permitir user namespaces sem privilégios.

As duas camadas do sandbox do Chromium, seccomp e namespaces, estão comprovadas nas sondas e no processo Chromium real: os renderers do CEF rodam no guest com `Seccomp: 2`, `NoNewPrivs: 1` e um filtro instalado pelo próprio Chromium. A renderização é por software: o image inclui o Mesa em `/run/opengl-driver`, o Xvfb oferece GLX, e o ANGLE usa o llvmpipe. Os smokes registram o renderer efetivo, `ANGLE (Mesa, llvmpipe ...)`, e conferem um pixel de WebGL lido de volta. O host passa `ignore-gpu-blocklist`, porque o blocklist do Chromium recusa WebGL no llvmpipe. Xorg sobre `/dev/dri/card0` é a próxima etapa gráfica.

O builder restringe a criação de user namespaces, então a sonda de namespaces não entra no `control`. Para executá-la em Linux:

```bash
docker run --rm --security-opt seccomp=unconfined --security-opt apparmor=unconfined -v "$PWD/userspace/compatibility/probes:/probes:ro" python:3.13-slim python -I /probes/namespaces.py
```

No guest, com o Desktop pronto, o CEF ocupa cerca de 1,1 GB e o `hostd` cerca de 100 MB, somando o RSS de seus processos, que conta mais de uma vez as páginas compartilhadas. No Linux de controle, o PSS do CEF fica em cerca de 640 MB. O guest não oferece `/proc/[pid]/smaps_rollup`, e os tempos de CPU de `/proc/[pid]/stat` não seguem a unidade do Linux, então esses dois valores do relatório do guest não servem para comparação.

A CI executa as sondas em Linux e aceita seleção de modo no workflow **Native baseline**. Para comparar com o OpenShell Live, registre a ISO amd64 e seu SHA-256 junto aos resultados.

## Shell com CEF

O launcher Rust inicia o `hostd` offline e abre o bundle Desktop em uma janela CEF. Ele reinicia o `hostd` na mesma porta quando ele falha e entrega o novo bootstrap ao host CEF pelo stdin, que fica aberto durante a sessão. A biblioteca Rust compartilhada valida a configuração e as mensagens do launcher e decide quais origens o Desktop e as abas do Browser podem navegar e carregar. O adaptador C++ chama a biblioteca e implementa os callbacks de janela e ciclo de vida do SDK CEF. Ele também responde aos comandos `/_shell/` da página do Desktop, que abrem e posicionam as abas do Browser como views sobrepostas, e oferece os recursos que o Electron fornece no OpenShell Live: downloads, perguntas de permissão e de arquivos, exceções de certificado, protocolo DevTools, PDF, capturas e clipboard. A [seção 10 do plano CEF](ARCHITECTURE-CEF.md#10-estado-da-implementação) descreve a ponte, os contextos, os limites conhecidos e o supervisor `openshell-session`.

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

## Atualização A/B

[`distro/ab`](distro/ab) implementa a transação da seção 6 do [plano](ARCHITECTURE.md). A [imagem experimental](distro/image) usa uma ESP com GRUB e quatro partições ext2, `system_a`, `system_b`, `recovery` e `data`.

- **Seletor.** Grava o estado de cada slot (`empty`, `staged`, `trial:N`, `good`, `bad`) e o slot confirmado em duas cópias, com geração e CRC-32. Cada mudança grava a cópia mais antiga, então uma gravação interrompida deixa a mais nova intacta.
- **Assinatura.** O manifesto vem com `<manifesto>.sig`: uma assinatura Ed25519, em base64, dos bytes exatos do manifesto, como o atualizador do OpenShell Desktop assina e confere seus releases. `stage` recebe a pasta das chaves confiáveis (PEM) e confere a assinatura antes de ler qualquer coisa do manifesto. `openshell-ab keygen` e `openshell-ab sign` servem às ferramentas de release e aos testes.
- **Atualização.** Torna o slot inativo não inicializável antes de escrever nele. Depois confere tamanho e SHA-256 de cada arquivo contra o manifesto do release, junto com a arquitetura e a faixa de schema dos dados, e só então o marca como `staged`.
- **Atualização pelo guest.** `openshell-ab <data>/openshell/ab update-mounted <origem> <manifesto> <schema> <chaves> <slot-montado>` valida a release assinada e agenda o próximo boot em teste. O comando exige que o slot em execução coincida com a release confirmada e que o destino seja a partição inativa ext2 do mesmo disco GPT da partição `data`. O GRUB identifica as partições da imagem por UUID. Use o diretório das chaves públicas confiáveis da instalação.
- **Boot.** O GRUB lê `grubenv` na partição `data` e grava o desconto antes de entregar o controle a um slot em teste. Após três tentativas sem confirmação, volta ao slot confirmado. O serviço de boot sincroniza esse resultado com o seletor. A imagem também prova o boot do recovery com o ambiente ausente ou sem um `boot.cfg` utilizável nos slots.
- **Confirmação.** A sessão instalada mantém o perfil em `data`. O renderer envia relatórios periódicos depois que o Desktop monta a interface e conclui uma leitura e escrita autenticadas pelo hostd. O supervisor exige 60 segundos de relatórios contínuos por padrão e reinicia a contagem quando perde os relatórios ou quando a sessão reinicia. O comando de confirmação verifica se o identificador da release em execução coincide com o registrado no slot.

### Canal de releases

O sistema instalado se atualiza por Configurações ▸ Atualizações, como o Live, mas a versão nova é o sistema inteiro. O build da ISO live também gera o canal em `out/native-*/native-channel/` ([channel.sh](distro/image/channel.sh)): `latest.json` com `.sig`, no formato do Desktop (`product: "openshell-native"`), e `openshell-native-<versão>-amd64.tar`, com kernel assinado, initramfs, `boot.cfg` e o `MANIFEST` do `openshell-ab` com sua assinatura. Publique a pasta em `<portal>/downloads/native/`. Uma chave Ed25519 do OpenShell Native assina os dois manifestos. Ela nasce no primeiro build, fica em `.secureboot/` com a chave do Secure Boot, e a imagem leva a parte pública em `/opt/openshell-media/release-keys`. Sem ela, os sistemas instalados não aceitam versões novas: guarde uma cópia.

1. O hostd confere a assinatura de `latest.json` e baixa o arquivo, com retomada e SHA-256, na caixa de entrada do sistema (`/data/openshell/updates/inbox`, do usuário da sessão).
2. **Instalar** pede ao supervisor `update-stage <arquivo>` ([update.rs](userspace/shell/src/update.rs)). Ele só registra o nome, num arquivo de root.
3. Ao reiniciar ou desligar pelo Desktop, e no boot seguinte se o computador parou antes, o init ([installed-session.sh](userspace/compatibility/probes/installed-session.sh)) desempacota o arquivo, monta o slot inativo e roda `openshell-ab update-mounted` com as chaves da imagem. Isso confere a assinatura do `MANIFEST` e cada arquivo, grava o slot e agenda o boot em teste. O resultado aparece em Atualizações (`update-status`).
4. O GRUB inicia a versão nova em teste, e a sessão a confirma depois de 60 s saudável. Se ela não iniciar três vezes, o GRUB volta à anterior.

O pendrive live não se atualiza: o Desktop mostra o link da ISO nova.

Os testes unitários simulam um corte de energia em cada gravação de uma atualização inteira. Em todos os casos, o boot seguinte escolhe um slot `good` ou em teste, nunca um slot incompleto. Também conferem a assinatura com o vetor 1 da RFC 8032. O [smoke](distro/ab/smoke.sh) repete o fluxo com o CLI sobre um diretório: recusa um manifesto alterado depois de assinado, um assinado por chave não confiável e um sem assinatura. Roda no Linux, na CI e no guest, em `/ext2`. O [smoke da sessão](tools/desktop/session-smoke.mjs) exige a confirmação do boot, uma única vez.

```bash
cargo test --manifest-path distro/ab/Cargo.toml
cargo build --release --manifest-path distro/ab/Cargo.toml
sh distro/ab/smoke.sh distro/ab/target/release/openshell-ab
```

O cenário `update` começa com apenas o slot A instalado e disponibiliza uma release assinada em `data`. O Asterinas monta o slot B, rejeita a atualização do slot ativo e a release sem assinatura, grava a release assinada e reinicia no slot B. A ação `desktop-image` usa um bundle com suporte ao relatório de saúde. O instalador para hardware segue pendente.

## Integração com o OpenShell

O Native consome o bundle Linux do Desktop gerado no repositório `openshell` por `pnpm desktop:bundle`, em `apps/desktop/dist-bundle/openshell-desktop`. O bundle precisa conter `BUILD.json`, `hostd/openshell-hostd.mjs` e `ui/index.html`. O `run.json` das ações `desktop`, `kiosk` e `apps` registra a versão, a data de build, a versão do Electron e o SHA-256 do hostd e do `ui/index.html`.

```bash
node tools/build/native.mjs desktop ../openshell/apps/desktop/dist-bundle/openshell-desktop
```

O hostd usa o Node e o Python do mesmo nixpkgs do guest. O `node-pty` do bundle é opcional, e o hostd usa o backend `python-pty` quando ele falta. Na ação `desktop`, a UI é servida e verificada por HTTP, sem renderer. A ação `kiosk` abre a mesma UI no shell. O Browser usa abas nativas quando o bundle inclui o adaptador `apps/desktop/src/host/nativeBrowser.tsx` do repositório `openshell`. Para validar o layout agrupado e a soltura real dos atalhos, gere o bundle com a versão do adaptador que negocia `surfaceLayout` e `shortcutPhases`.

```bash
node tools/build/native.mjs kiosk ../openshell/apps/desktop/dist-bundle/openshell-desktop
```

Para compilar e abrir a UI real no launcher:

```bash
bash tools/desktop/build-cef.sh /path/to/cef-sdk
out/cef-build/rust/release/openshell-shell /opt/openshell \
  out/cef-build/host/runtime/openshell-cef-host "$HOME/.local/state/openshell-native"
```

O bundle em `/opt/openshell` precisa conter `hostd/openshell-hostd.mjs` e `ui/index.html`. O launcher usa o Node incluído no bundle ou o Node disponível no PATH. Ele usa a porta local `7788` por padrão. Defina `OPENSHELL_NATIVE_PORT` antes do primeiro boot de um perfil se precisar de outra porta.

`node tools/desktop/cef-smoke.mjs <sample|host|isolation> <cef-executable>` executa uma prova gráfica individual. Os resultados ficam em `out/cef-*/cef-smoke.json`.

O modo `isolation` abre duas views na mesma janela X11, com contextos independentes e dois servidores de teste locais. O teste exige entrada em ambas as views antes e depois de redimensionar a janela. Também verifica cookies e bloqueio de requisições diretas, redirecionadas e iniciadas por Worker. A prova usa views lado a lado.

`node tools/desktop/shell-smoke.mjs <openshell-shell> <desktop-bundle> <cef-host>` dirige o Desktop real em uma sessão X11 de usuário comum. `node tools/desktop/session-smoke.mjs <openshell-session> <openshell-shell> <desktop-bundle> <cef-host> <Xvfb> <openbox>`, executado como root, inicia a sessão pelo supervisor.


## Desktop de referência com Electron em X11

Em um guest Linux com Xorg já ativo, execute com um usuário comum. Passe a pasta do bundle do OpenShell Desktop e o executável Electron Linux correspondente ao `BUILD.json` do bundle.

```text
node tools/desktop/smoke.mjs /opt/openshell /opt/openshell/electron/electron
```

O teste usa perfil descartável, hostd offline e uma página local com Web Worker no Browser. O relatório exige o preload do Desktop, a ponte hostd, o resultado do worker e evidência de filtros seccomp nos processos renderer. Para diagnosticar falhas, consulte `native-desktop.json`, o relatório do Electron e o log em `out/desktop-*/`.

A revisão fixada do Asterinas omite a syscall `seccomp` na tabela x86-64, e o patch 0002 a adiciona junto com os campos de `/proc/[pid]/status` que este teste lê. O teste `sandbox` exige que o guest instale e aplique um filtro e que entre em novos namespaces. O resultado dessa prova condiciona a integração do kiosk. [Tabela de syscalls da revisão fixada](https://github.com/asterinas/asterinas/blob/c4b7ed575b7b32773475b194d2ad41fb6d03765b/kernel/core/src/syscall/arch/x86.rs)

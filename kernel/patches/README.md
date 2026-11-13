# Série de patches do kernel

Os patches aplicam em ordem de nome sobre a revisão do Asterinas fixada em [upstream.lock.json](../config/upstream.lock.json). O build aplica a série do zero em cada execução e registra o SHA-256 de cada arquivo em `toolchain.txt`.

| Patch | Workload que exige a mudança | Teste | Situação upstream |
| --- | --- | --- | --- |
| `0001-lock-osdk-dependencies` | Build reproduzível do kernel. Sem os lockfiles, o nightly fixado resolve `unwinding` 0.2.11 e o build falha. | Check `kernel-build`. O `kernel-Cargo.lock` do relatório mostra a resolução usada. | Não enviado. Ajuste de build local. |
| `0002-add-seccomp-filters` | Sandbox do Chromium no Electron e no CEF. O renderer instala filtros seccomp-bpf e trata `SIGSYS`. | [seccomp.c](../../userspace/compatibility/probes/seccomp.c), check `native-seccomp`. A mesma sonda passa no Linux de controle (`control-seccomp`). | Não enviado. A v0.19.0 não tem a syscall `seccomp` nem `PR_SET_SECCOMP`. |
| `0003-report-renames-to-inotify` | `fs.watch` do Node, usado pelo hostd em Files e Memory. Sem os eventos de `rename`, uma gravação atômica nunca chega ao watcher. | Sonda `fileEvents` de [node.mjs](../../userspace/compatibility/probes/node.mjs), check `native-compat`. [inotify.py](../../userspace/compatibility/probes/inotify.py) reduz o caso a `inotify` e `epoll`. | Não enviado. A v0.19.0 define `IN_MOVED_FROM` e `IN_MOVED_TO` e nunca os publica. |
| `0004-add-user-namespaces` | Camada de namespaces do sandbox do Chromium, criada por um processo sem privilégios. | [namespaces.py](../../userspace/compatibility/probes/namespaces.py), passos `enter` e `identity`. Check `native-namespaces`. | Não enviado. A v0.19.0 rejeita `CLONE_NEWUSER` com `EINVAL`. |
| `0005-add-pid-namespaces` | O zygote do Chromium isola os renderers em um namespace de PID próprio. | `namespaces.py`, passos `pid` e `outside`. | Não enviado. A v0.19.0 rejeita `CLONE_NEWPID`. |
| `0006-add-network-namespaces` | Renderers sem acesso à rede nem a sockets abstratos de outros processos, como o do X11. | `namespaces.py`, passos `network` e `outside`. | Não enviado. A v0.19.0 rejeita `CLONE_NEWNET` e tem uma única tabela de nomes abstratos. |
| `0007-check-port-privileges-per-network-namespace` | O hostd escuta em uma porta efêmera de loopback como usuário comum. A v0.19.0 inclui a porta 0 entre as privilegiadas e recusa o `listen` com `EACCES`. | [hostd-smoke.mjs](../../tools/desktop/hostd-smoke.mjs) no guest, check `native-hostd`. | Não enviado. Correção de bug. |
| `0008-give-terminals-to-their-opener` | O Terminal do Desktop abre um PTY como usuário comum. O slave nascia com dono root e modo 0620, e o `grantpt` da libc falhava. | `hostd-smoke.mjs`, verificação `terminal`. | Não enviado. Correção de bug. |
| `0009-keep-local-pids-while-referenced` | O zygote do Chromium, PID 1 do seu namespace, espera o helper que troca a raiz. O `wait4` devolvia 0 porque o 0005 apagava o PID local ao recolher o filho, antes de convertê-lo. | [chromium-userns.c](../../userspace/compatibility/probes/chromium-userns.c), verificação `zygote_sandboxed`. Check `native-chromium-userns`. | Não enviado. Correção do 0005. |
| `0010-count-threads-in-task-links` | O Chromium conta as threads pelo número de links de `/proc/self/task` e aborta com `CHECK` quando o processo não parece ter thread única. Todo diretório do procfs informava 2 links. | `chromium-userns.c`, verificação `thread_count`. | Não enviado. A v0.19.0 não informa as threads nos links. |
| `0011-expose-pid-and-network-namespace-files` | O Chromium só cria os tipos de namespace cujo arquivo em `/proc/self/ns` existe. Sem `pid` e `net`, o zygote nascia só com o user namespace e abortava. | `chromium-userns.c`, verificação `namespace_files`. | Não enviado. Complementa o 0005 e o 0006. |
| `0012-limit-unix-sockets-in-flight` | O Mojo, IPC do Chromium, passa sockets Unix por `SCM_RIGHTS`. A v0.19.0 só permitia isso com `CAP_SYS_ADMIN` no namespace inicial, e os renderers sem privilégios nunca recebiam seu canal. | `chromium-userns.c`, verificação `socket_passing`, que também comprova o limite. | Não enviado. Substitui a exigência de privilégio por um limite por usuário. |
| `0013-honor-msg-dontwait` | O serviço de rede do Chromium lê o dump de endereços e interfaces do netlink com `recv(MSG_DONTWAIT)` até `EAGAIN`. A v0.19.0 ignorava a flag, o recebimento bloqueava, e o browser esperava para sempre antes de criar a janela. | `chromium-userns.c`, verificação `address_tracker`. | Não enviado. A v0.19.0 só emitia um aviso para a flag. |
| `0014-lay-out-environment-like-linux` | Os filhos do zygote reescrevem o título sobre as strings de argumento e ambiente, que precisam estar contíguas e em ordem. A v0.19.0 gravava o ambiente em ordem inversa, e `/proc/[pid]/cmdline` não identificava os renderers. | `chromium-userns.c`, verificação `process_title`, e o `cef-smoke`, que encontra os renderers por esse título. | Não enviado. |
| `0015-inherit-address-reuse-in-accepted-connections` | O launcher reinicia o hostd na mesma porta, que o perfil do Desktop exige. As conexões aceitas pelo hostd anterior não herdavam `SO_REUSEADDR` e, enquanto fechavam, o novo `listen` falhava com `EADDRINUSE`. | [python.py](../../userspace/compatibility/probes/python.py), sonda `server_restart`, e o [smoke do shell](../../tools/desktop/shell-smoke.mjs), verificação `hostd-restart`. | Não enviado. Correção de bug. |
| `0016-ignore-wake-ups-of-finished-tasks` | Os processos do CEF criam e encerram muitas threads. Uma thread acordada enquanto terminava voltava a uma fila de execução depois de `exit_current` e, escolhida de novo, saía de `exit_current`: o kernel entrava em pânico com `unreachable!()` no meio da prova de isolamento do CEF. | Check `native-cef-isolation` e as fases seguintes da ação `apps`. A corrida é rara e não tem sonda própria. | Não enviado. Correção de bug, ligada ao problema 2 de [asterinas#1633](https://github.com/asterinas/asterinas/issues/1633). |
| `0017-do-not-wait-for-the-address-space-in-atomic-page-faults` | Com o perfil de apps (Xpra, o tema de cursor, `xsettingsd`), o kernel entrou em pânico quando uma cópia de memória de usuário feita com spinlocks seguros faltou página enquanto outro processo segurava o espaço de endereços: o tratador esperou pelo `RwMutex`, e `might_sleep` derrubou o sistema. | Check `native-apps`, que deixou de rodar. A corrida não tem sonda própria. | Não enviado. Correção de bug. O chamador que copia com spinlocks seguros ainda não foi identificado. |

## 0002 · seccomp

O patch adiciona `seccomp(2)`, `prctl(PR_GET_SECCOMP)` e `prctl(PR_SET_SECCOMP)`, o modo estrito e filtros BPF clássicos avaliados na entrada de toda syscall. `/proc/[pid]/status` passa a informar `NoNewPrivs`, `Seccomp` e `Seccomp_filters`, que os smokes do Desktop leem para comprovar a filtragem dos renderers.

Invariantes:

- O estado é por thread. Threads e processos novos herdam o estado de quem os criou, e `execve` o preserva. Filtros só se acumulam: não existe operação que remova um filtro.
- Instalar um filtro exige `no_new_privs` ou `CAP_SYS_ADMIN` no user namespace da thread.
- O validador aceita somente as instruções que o Linux aceita para seccomp. Saltos só avançam, ficam dentro do programa e o programa termina em `RET`, então a avaliação sempre termina. Leituras ficam dentro de `seccomp_data`.
- Todos os filtros da cadeia rodam e vale a ação mais restritiva.
- `SECCOMP_FILTER_FLAG_TSYNC` aplica a nova cadeia a todas as threads com o conjunto de tasks travado. `clone` copia o estado de novo sob a mesma trava, então uma thread criada durante a sincronização recebe o filtro.
- `SECCOMP_RET_TRAP` entrega `SIGSYS` com os registradores da entrada da syscall. Se o sinal estiver bloqueado ou ignorado, o processo termina.

Diferenças em relação ao Linux:

- `SECCOMP_RET_TRACE` e `SECCOMP_RET_USER_NOTIF` falham com `ENOSYS`, como no Linux sem tracer e sem listener. `SECCOMP_FILTER_FLAG_NEW_LISTENER` retorna `EINVAL`.
- `SECCOMP_RET_LOG` permite a syscall sem registrar em log de auditoria.
- `SECCOMP_GET_ACTION_AVAIL` informa somente as ações efetivamente aplicadas.

## 0003 · eventos de rename

`rename` publica `IN_MOVED_FROM` no diretório de origem e `IN_MOVED_TO` no de destino com o mesmo cookie, e `IN_MOVE_SELF` no inode movido. Um inode substituído recebe `IN_ATTRIB` e, sem links restantes, `IN_DELETE_SELF`. `RENAME_EXCHANGE` informa dois movimentos.

## 0004 · user namespaces

Um processo sem privilégios cria um user namespace com `clone` ou `unshare`, e passa a ter todas as capabilities nele. O dono do namespace é o `euid` de quem o criou.

Invariantes:

- O kernel continua guardando UIDs e GIDs do namespace inicial. Os mapas aceitam somente uma faixa de identidade (`N N count`), então um ID visível no namespace é o mesmo ID fora dele. Um ID fora da faixa não está mapeado.
- Cada mapa é gravado uma vez, por uma thread do namespace ou do pai. Sem privilégio no pai, a thread só mapeia o próprio `euid` e só mapeia o próprio `egid` depois de gravar `deny` em `setgroups`. Mapear outros IDs exige `CAP_SETUID` ou `CAP_SETGID` no pai.
- A checagem de capabilities segue `cap_capable` do Linux: a thread usa suas capabilities efetivas no próprio namespace, o dono de um namespace filho tem todas nele, e capabilities valem nos descendentes. Fora disso, nenhuma.
- `CAP_DAC_OVERRIDE`, `CAP_SETFCAP`, set-user-ID e set-group-ID só valem para inodes cujo dono e grupo estão mapeados no namespace da thread. `setuid`, `setgid`, `setgroups` e as variantes recusam IDs não mapeados.
- Criar uma instância de filesystem continua exigindo `CAP_SYS_ADMIN` no namespace inicial, porque o kernel ainda não separa os tipos seguros para um user namespace.

## 0005 · PID namespaces

Cada processo e cada thread têm um ID global, o ID no namespace inicial, e um ID local em seu namespace e em cada ancestral. Os IDs locais existem somente na fronteira das syscalls: o kernel converte o ID recebido de `getpid`, `kill`, `wait4`, `waitid`, `tgkill`, `setpgid`, `getpgid`, `getsid`, `prlimit64`, prioridades, afinidade, timers, `pidfd_open`, `capget`, `capset`, `fcntl(F_SETOWN)`, `clone` com `CLONE_PARENT_SETTID`, `set_tid_address`, futex robusto, ioctls de job control, `siginfo` e `SCM_CREDENTIALS`.

Invariantes:

- A tabela de PIDs esconde de uma thread as entradas de namespaces que não descendem do dela. Uma conversão esquecida, portanto, não alcança um processo de fora.
- O primeiro processo de um namespace tem PID 1, adota os órfãos do namespace, e sua saída mata os demais processos e impede a entrada de novos.
- `unshare(CLONE_NEWPID)` muda o namespace dos próximos filhos e só pode ser usado uma vez por processo.

Diferenças em relação ao Linux:

- `/proc` lista os IDs globais dos processos visíveis, e `ptrace` recebe IDs globais. A tabela de PIDs continua impedindo o acesso a processos de fora. O sandbox do Chromium remove `/proc` da visão dos renderers antes de usá-los.

## 0006 · network namespaces

Um namespace de rede tem suas interfaces, suas tabelas de rota e seus nomes abstratos de socket Unix. O inicial usa as interfaces encontradas no boot. Um novo começa somente com um loopback próprio, então os processos dele não alcançam a rede nem outros namespaces.

Diferenças em relação ao Linux:

- O socket usa o namespace da thread que o opera, não o de quem o criou. Os dois só divergem se um socket for passado a um processo de outro namespace de rede.
- O loopback de um namespace novo recebe o próximo índice global de interface, e não o índice 1.
- A thread que atende o loopback continua ativa depois que o último processo do namespace termina.

## 0007 e 0008 · correções

A porta 0 pede uma porta efêmera e não exige `CAP_NET_BIND_SERVICE`. As portas de 1 a 1023 exigem a capability no user namespace dono do namespace de rede. O slave de um PTY pertence ao `fsuid` e ao `fsgid` de quem abriu `/dev/ptmx`, como no Linux.

## 0009 a 0011 · o que o Chromium verifica

- O ID local de um processo pertence ao `Process` e o de uma thread secundária à `PosixThread`. Ele continua válido enquanto o objeto existir, e um processo recolhido ainda tem o PID que o `wait4` informa. Os IDs globais não são reutilizados, então um ID local não se confunde com o de um processo novo.
- `/proc/[pid]/task` informa 2 links mais um por thread, como `proc_task_getattr` do Linux. Os demais diretórios do procfs continuam com 2.
- `/proc/[pid]/ns` lista `pid`, `pid_for_children` e `net`, além dos tipos que a v0.19.0 já tinha.

## 0012 · sockets Unix em trânsito

Um socket Unix enviado dentro de um ciclo, por exemplo pelo próprio canal, continua vivo depois que o último descritor é fechado. O Linux coleta esses ciclos. O Asterinas ainda não tem coletor, e a v0.19.0 evitava o problema exigindo `CAP_SYS_ADMIN`, o que impede o IPC de qualquer aplicação multiprocesso comum.

O patch cobra os sockets Unix de cada mensagem `SCM_RIGHTS` ao UID real de quem envia e limita o total em trânsito ao `RLIMIT_NOFILE` do remetente, como `too_many_unix_fds` do Linux. Acima do limite, o envio falha com `ETOOMANYREFS`, salvo com `CAP_SYS_RESOURCE` ou `CAP_SYS_ADMIN`. A cobrança é liberada quando o destinatário recebe os descritores ou quando a mensagem é descartada.

O risco que continua aberto: um ciclo deliberado mantém até `RLIMIT_NOFILE` sockets de um usuário vivos até o desligamento. O limite impede que um usuário esgote a memória do kernel com isso, mas não recupera esses sockets. O coletor de ciclos fica como trabalho futuro.

## 0013 e 0014 · sockets e título de processo

- `MSG_DONTWAIT` torna uma única chamada de envio ou recebimento não bloqueante em sockets IP, Unix, netlink e vsock, independentemente do modo do arquivo. `MSG_NOSIGNAL` passa a constar como suportada: o kernel nunca envia `SIGPIPE` em sockets.
- As strings de ambiente ficam logo depois das de argumento e em ordem crescente, como no Linux. `/proc/[pid]/cmdline` segue `get_mm_cmdline`: se o último byte da área de argumentos não é NUL, o título continua na área de ambiente até o primeiro NUL.

## 0015 · reinício de servidores

Como no Linux, uma conexão aceita herda de quem escuta o `SO_REUSEADDR` no momento do `accept`. Um servidor reiniciado com `SO_REUSEADDR` volta a fazer `bind` na porta enquanto as conexões do anterior terminam de fechar. A série não altera o caso de dois sockets em `listen` na mesma porta: como já era na v0.19.0, ele continua permitido quando todos os sockets da porta usam `SO_REUSEADDR`. O Linux recusa esse caso.

## 0016 · fim de tarefa

`Waker::wake_up` marca a tarefa como acordada e só depois a põe numa fila. Nesse intervalo, a tarefa pode seguir sem dormir até o fim da sua função, e `exit_current` a tira da fila e limpa a CPU dela. A fila recebia então uma tarefa terminada, e o escalonador voltava a ela. O patch marca a tarefa como terminada quando a função dela retorna, em `kernel_task_entry`, e o `enqueue` do escalonador recusa a tarefa marcada depois de obter a CPU dela. A marca não usa `Thread::is_exited`, porque a saída POSIX ainda pode dormir depois de marcar a thread, ao fechar arquivos e encerrar o processo.

## 0017 · faltas de página em modo atômico

Uma cópia de ou para a memória de um processo pode faltar página. O ostd chama então o tratador do kernel, que lê o espaço de endereços sob um `RwMutex`. Com spinlocks seguros (modo atômico), esperar é proibido: se outro processo segurava o mutex, a espera chamava `might_sleep`, e o kernel entrava em pânico (`This function might break atomic mode`). Livre, o mutex é tomado sem esperar, e a falta é tratada como sempre. O patch expõe `in_atomic_mode` do ostd e, em modo atômico, toma o espaço de endereços só se estiver livre (`try_read`); ocupado, a cópia falha e a chamada de sistema reporta o erro. Uma primeira versão recusava toda falta em modo atômico e quebrou o guest, porque esses acessos são comuns e quase sempre encontram o mutex livre. O chamador que copia com spinlocks seguros ainda precisa ser identificado e corrigido.

## Diagnóstico de falhas do Chromium

Builds oficiais do Chromium e do CEF disparam `int3` em um `CHECK` sem escrever a condição, e quase não emitem logs. Os problemas desta série foram localizados com instrumentação temporária do kernel, que não faz parte dos patches:

- No sinal de falha, registrar o RIP e o mapeamento de arquivo que o contém. O deslocamento resolvido com o `libcef.so` do SDK, que mantém os símbolos, aponta o `CHECK`.
- Para syscalls que bloqueiam mais de 0,7 s na thread principal dos processos CEF, registrar número, argumentos, retorno e a cadeia de frame pointers da pilha de usuário. O Chromium compila com frame pointers, então a pilha resolve até a função que espera.
- Registrar saídas de processo com status diferente de zero e os nomes das threads no `exit_group`, que identificam o tipo de processo do Chromium.

Com `LOG_LEVEL=warn`, o kernel também informa syscalls não implementadas e os sinais que encerram processos. A comparação com o mesmo host CEF no Linux do builder separa o que é do kernel do que é do ambiente.

## Validação dos namespaces

A sonda prepara, ainda no namespace inicial, um arquivo do root com modo 0600, um socket abstrato e um processo. Em seguida, sem privilégios, ela entra nos novos namespaces e exige que o arquivo, o socket e o processo fiquem inacessíveis, que `setuid(0)` e uma segunda gravação de `uid_map` falhem e que um namespace não possa ser criado depois do descarte das capabilities. A mesma sonda passa no Linux de controle.

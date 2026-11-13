# Bloqueio de tela

O bloqueio de tela do OpenShell Native é o do OpenShell Live: o XSecureLock
cobre a tela e segura teclado e mouse, e a tela de bloqueio do OpenShell é o
módulo de autenticação dele. Muda só quem confere a senha: no Live, o PAM do
Debian (`authproto_pam`); aqui, o supervisor da sessão, que guarda o hash da
senha escolhida no instalador (`userspace/shell`, `login.rs`). O boot também
abre nesta tela: a entrada do sistema instalado é a mesma tela de bloqueio.

| Arquivo | O que é |
| --- | --- |
| `lock_screen.py`, `lock_view.py`, `lock_auth.py`, `lock_settings.py` | A tela de bloqueio do Live, sem mudanças (openshelllive `f27d2db`, `live/config/includes.chroot/usr/local/lib/openshell/`). O `initramfs.nix` troca os caminhos do Debian pelos do Nix: o XSecureLock, o `setxkbmap`, o ajudante de senha (`openshell-authproto` no lugar do `authproto_pam`) e o de energia (`openshell-power` no lugar do `systemctl`), e lê as escolhas de Configurações ▸ Tela de bloqueio e a foto da conta no `$HOME` da sessão, não na pasta pessoal do `/etc/passwd`. Para atualizar, copie os arquivos do Live de novo. |
| `lock.sh` | `openshell-lock`: abre o XSecureLock com a tela de bloqueio e as mesmas opções do `lock.sh` do Live, e fica em primeiro plano até desbloquear. |
| `lock-ready.sh` | O XSecureLock o roda quando a tela e o teclado já estão presos: grava o pid em `$XDG_RUNTIME_DIR/openshell-locked`, onde o Desktop vê que a tela está bloqueada, e mostra o campo de senha. |
| `numlock.py` | O ajudante de Num Lock do Live, sem mudanças: segue os modificadores do XKB (também enquanto o XSecureLock segura o teclado), guarda a escolha em `$XDG_STATE_HOME/openshell/numlock` e a aplica de novo no próximo boot, antes da primeira senha. |
| `openshell-wm.sh` | `openshell-wm`, o gerenciador de janelas da sessão (`wm=` do supervisor): inicia o `numlock.py` e depois o Openbox, no mesmo grupo de processos, antes do greeter e da tela de bloqueio. |

Quem bloqueia é o serviço de bloqueio do hostd (`apps/desktop/hostd/services/lock.ts`):
o botão Bloquear e o tempo sem uso escolhido em Configurações ▸ Tela de
bloqueio. No Live, o tempo sem uso é do `xss-lock`, que precisa do logind; no
Native, o Desktop informa o tempo sem uso ao hostd.

Duas peças do sistema tornam isso possível no Native:

- `openshell-authproto` (`userspace/shell/src/bin`): fala o protocolo de
  autenticação do XSecureLock e pergunta ao supervisor pelo socket do sistema
  (`login <senha>`). Uma senha errada espera 2 s, como no login.
- O patch de kernel 0024: a tela de bloqueio guarda a senha só em memória
  travada (`mlock`), e o kernel passou a aceitar essas chamadas (não há swap).

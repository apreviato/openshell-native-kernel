# Wine no OpenShell Native — matriz de compatibilidade

Estado das fases do [plano de Wine](../../../OPENSHELL_WINE.md). A ação `wine` de `tools/build/native.mjs` monta o perfil de apps Windows sobre o de apps Linux e roda as provas abaixo no Linux de controle e no guest Asterinas.

## Versões fixadas

| Peça | Versão | Origem |
| --- | --- | --- |
| Wine | 11.0, 64 bits com WoW64 (`wineWow64Packages.stable`) | nixpkgs fixado em `distro/nixpkgs.nix` |
| Kernel | Asterinas v0.19.0 (`c4b7ed5`) com os patches 0001 a 0017 | [kernel/patches](../../../kernel/patches/README.md) |
| CEF | 147.0.10 | nixpkgs fixado |
| Xpra e cliente HTML5 | 6.4 e 17.1 | nixpkgs fixado |

Wine Mono e Gecko não fazem parte do prefixo padrão: ele é criado sem eles (`WINEDLLOVERRIDES=mscoree,mshtml=`). Um programa .NET recebe o Wine Mono no próprio prefixo (W5); programas que embutem o motor HTML do Windows (Gecko) seguem fora.

## Execução

- [openshell-wine.sh](openshell-wine.sh) é o runner do perfil Wine. A abertura avulsa usa o prefixo `default`, e o fluxo de instalação cria um prefixo por app em `~/.local/share/openshell/wine/prefixes`. O prefixo é criado sem display com `wineboot`. O runner remove o mapeamento Z: e mapeia a pasta do arquivo aberto como P:. O processo roda com as permissões do usuário da sessão, inclusive para acessos fora dos mapeamentos de drives.
- O Desktop reconhece `.exe` e `.msi` (`apps/desktop/src/host/openWith.ts`, no repositório `openshell`). Abrir um desses arquivos pelo Files ou pelo `open` do Terminal mostra o seletor, com o Wine recomendado; a execução é sempre uma escolha explícita.

## Provas

| Prova | Como | Linux de controle | Asterinas |
| --- | --- | --- | --- |
| `wine --version`, `wineboot -i` num prefixo isolado | [wine-smoke.mjs](../../../tools/desktop/wine-smoke.mjs) | Passa: prefixo em 13 s | Pendente |
| Notepad abre em X11 e fecha com Alt+F4 | `wine-smoke.mjs` | Passa: janela em 0,8 s | Pendente |
| Nada do prefixo fica para trás, `wineserver` incluído | `wine-smoke.mjs` | Passa | Pendente |
| Um `.exe` aberto pelo Terminal escolhe o Wine e abre numa janela do Desktop (Xpra) | fase `linux-apps` do [smoke do shell](../../../tools/desktop/shell-smoke.mjs), com `OPENSHELL_SMOKE_WINE=1` | Passa | Pendente |
| O prefixo do OpenShell não tem a unidade Z: | idem | Passa | Pendente |
| Fechar a janela encerra o Wine | idem | Passa | Pendente |
| W4: um instalador MSI ([probe.wxs](probe.wxs)) instala num prefixo próprio, com atalho no Desktop e sem associações de arquivo do Wine | `wine-smoke.mjs`, com `OPENSHELL_WINE_PROBE_MSI` | Passa | Pendente |
| W4: o programa abre pelo atalho, como o Desktop o abre, e a configuração sobrevive entre execuções | idem | Passa | Pendente |
| W4: backup, remoção (com o atalho) e restauração do prefixo, com a configuração | idem | Passa | Pendente |
| W4: outro Wine rodou o prefixo por último: backup antes da atualização, e a versão registrada | idem | Passa | Pendente |
| W4: desinstalação pelo MSI, e remoção do prefixo sem processos restantes | idem | Passa | Pendente |

## Estado das fases

| Fase | Estado |
| --- | --- |
| W0 · inventário e baseline | Versões fixadas e provas no Linux de controle. |
| W1 · Wine em userspace | Passa no Linux de controle; o guest roda a mesma prova na ação `wine`. |
| W2 · abrir `.exe` | Associação de `.exe` e `.msi`, runner em primeiro plano, limpeza pelo `hostd` ao fechar (a sessão Xpra inteira). |
| W3 · janelas no Desktop | Pelo caminho Xpra dos apps Linux: a janela do programa é uma janela do OpenShell, com dock, foco, teclado e clipboard dos apps Linux. |
| W4 · prefixos por app, instalação | Runner e painel de Configurações oferecem prefixos por instalação, logs, backup e restauração. O gate W4 do `wine-smoke` passa no Linux de controle. No Asterinas, roda na ação `wine` e segue pendente. Os prefixos ficam nos dados da pessoa (`XDG_DATA_HOME`), fora dos slots que uma atualização ou um rollback substitui. |
| W5 · compatibilidade avançada | Em andamento. Programas .NET: o runner reconhece o cabeçalho CLR do PE (PE32 e PE32+) e instala o Wine Mono do pacote do Wine só no prefixo desse programa, na primeira execução; os outros prefixos seguem sem ele. Faltam as provas no `wine-smoke` (Win32 por WoW64, .NET), o Gecko e os VC runtimes sob demanda e as medições de lançamento e RAM. |

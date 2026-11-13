# Wine no OpenShell Native — matriz de compatibilidade

Estado das fases do [plano de Wine](../../../OPENSHELL_WINE.md). A ação `wine` de `tools/build/native.mjs` monta o perfil de apps Windows sobre o de apps Linux e roda as provas abaixo no Linux de controle e no guest Asterinas.

## Versões fixadas

| Peça | Versão | Origem |
| --- | --- | --- |
| Wine | 11.0, 64 bits com WoW64 (`wineWow64Packages.stable`) | nixpkgs fixado em `distro/nixpkgs.nix` |
| Kernel | Asterinas v0.19.0 (`c4b7ed5`) com os patches 0001 a 0017 | [kernel/patches](../../../kernel/patches/README.md) |
| CEF | 147.0.10 | nixpkgs fixado |
| Xpra e cliente HTML5 | 6.4 e 17.1 | nixpkgs fixado |

Wine Mono e Gecko não fazem parte do perfil: o prefixo é criado sem eles (`WINEDLLOVERRIDES=mscoree,mshtml=`), e programas .NET ou que embutem o motor HTML do Windows ficam fora até a fase W5.

## Execução

- [openshell-wine.sh](openshell-wine.sh) é o runner que a entrada `wine.desktop` do perfil usa para `.exe` e `.msi`. O programa roda em primeiro plano, e a sessão Xpra dura o mesmo que ele. O prefixo é do OpenShell (`~/.local/share/openshell/wine/prefixes/default`), criado na primeira vez com `wineboot`. A unidade Z:, que o Wine mapeia para o sistema de arquivos inteiro, é removida: o programa vê só C:. O programa aberto alcança só a própria pasta, como unidade P:. O prefixo é criado sem display: a janela de progresso do `wineboot` sumiria antes da do programa, e o Desktop encerra uma sessão cujas janelas todas fecharam.
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

## Estado das fases

| Fase | Estado |
| --- | --- |
| W0 · inventário e baseline | Versões fixadas e provas no Linux de controle. |
| W1 · Wine em userspace | Passa no Linux de controle; o guest roda a mesma prova na ação `wine`. |
| W2 · abrir `.exe` | Associação de `.exe` e `.msi`, runner em primeiro plano, limpeza pelo `hostd` ao fechar (a sessão Xpra inteira). |
| W3 · janelas no Desktop | Pelo caminho Xpra dos apps Linux: a janela do programa é uma janela do OpenShell, com dock, foco, teclado e clipboard dos apps Linux. |
| W4 · prefixos por app, instalação | Pendente: hoje há um prefixo único do OpenShell. |
| W5 · compatibilidade avançada | Pendente: Wine Mono, Gecko, VC runtimes sob demanda e medições. |

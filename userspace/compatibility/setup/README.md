# Tela de configuração da mídia live

O menu de boot do pendrive do OpenShell Native é o do OpenShell Live
(`distro/image/build-media.sh`):

| Entrada | O que inicia |
| --- | --- |
| Start OpenShell Native Live | O Desktop live, como antes. É a padrão, depois de cinco segundos. |
| Install OpenShell Native on this computer | Esta tela, no modo `install`: fuso horário, teclado, disco, conta, resumo e a cópia, como o Calamares pergunta no Live. |
| Update installed OpenShell Native (keep files) | Esta tela, no modo `update`: as instalações encontradas nos discos, a atualização pelo pendrive ou a volta ao release anterior. Só aparece numa mídia feita com a chave de release. |
| More options | A chave do Secure Boot (MokManager), as configurações do firmware UEFI, reiniciar e desligar. |

As duas entradas novas não iniciam o Desktop. O `installed-session.sh` sobe só o X, o Openbox e esta tela, sem nenhum serviço da sessão. "Try OpenShell Native", na tela, sai dela e inicia o Desktop live.

| Arquivo | O que é |
| --- | --- |
| `openshell_setup.py` | A tela, em GTK, com a aparência do atualizador do Live (`update_gui.py`, cujo CSS é o mesmo). Roda como o usuário da sessão sob o supervisor (`userspace/shell`), que faz o trabalho como root e liga e desliga o computador. |
| `setup_model.py` | O que a tela sabe sem janela: as respostas do supervisor (`install.rs`, `usb_update.rs`, `power.rs`); os fusos de `/usr/share/zoneinfo/zone1970.tab` e os layouts de `/usr/share/X11/xkb/rules/base.lst`, os mesmos arquivos que Configurações usa; e a ordem entre releases. |
| `test_setup_model.py` | Os testes do modelo (`python3 -m unittest test_setup_model`), sem GTK. |

O trabalho fica com dois programas da mídia, que o supervisor roda como root:

- `openshell-native-install` (`distro/image/install.sh`): apaga o disco escolhido e grava nele o sistema da memória. Antes, quando a mídia tem manifesto, confere se o que o computador leu do pendrive é o que foi gerado (`medium.sh`, `release_intact`).
- `openshell-native-update` (`distro/image/update.sh`): grava o release da mídia no slot do sistema instalado que não está em uso (`openshell-ab update-offline`), que confere o manifesto assinado e cada arquivo gravado. Na partida seguinte, o novo release roda em teste. A sessão o confirma, e três partidas sem sucesso voltam ao release anterior. Os dados, a conta e as configurações ficam como estavam. "Put back the previous release" põe em teste o release de antes da última atualização (`openshell-ab restore`).

O manifesto assinado do release vai na linha de comando do kernel da mídia (`openshell.manifest`, `openshell.manifest_sig`), não no initramfs, porque ele cobre o próprio initramfs.

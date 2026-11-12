# OpenShell Native — Plano de ação para integração de aplicações Windows via Wine

**Versão:** 1.0  
**Data:** 12 de novembro de 2026  
**Estado:** proposta executável; implementação de Wine no Asterinas **não validada**  
**Base:** `OPENSHELL_NATIVE_ARCHITECTURE_FINAL.md` e *OpenShell Native — Migração do Electron para CEF*  
**Alvo inicial:** x86-64, Asterinas + Linux ABI, OpenShell shell em Rust/CEF, apps Debian via X11/Xpra, Wine no userspace.

> **Decisão:** adicionar compatibilidade Windows como um runtime opcional gerenciado pelo mesmo App Manager usado para aplicações Linux. Manter Windows PE/Win32 fora do kernel, preservar a integração gráfica Debian e não modificar o frontend principal além de adaptadores e UX específicos.

## 1. Resultado esperado

Ao abrir um `.exe` pelo Files ou App Launcher, OpenShell deve detectar o formato PE, selecionar Wine, preparar um prefix isolado, lançar o processo sob um usuário não privilegiado e apresentar a janela dentro do desktop web OpenShell. A janela participa de dock, foco, resize, minimize, workspace e encerramento como uma aplicação Debian. Os dados e permissões sobrevivem ao fechamento da sessão, sem conceder acesso irrestrito a arquivos do usuário.

A primeira entrega **não** promete que todo `.exe` será compatível. Suporte a PE/Win32 depende simultaneamente da implementação Wine, das bibliotecas do userspace Debian e da semântica Linux disponível no Asterinas.

### 1.1 Requisitos obrigatórios

- Preservar a execução de aplicações Debian ELF, GTK/Qt e CLI, incluindo a integração existente via Xpra.
- Manter CEF multiprocesso e seu sandbox separados do Wine e de executáveis não confiáveis.
- Usar o mesmo `Surface Registry` e o mesmo modelo lógico de janelas para apps Linux e Windows.
- Não adicionar runtime Wine à inicialização padrão do desktop; inicializar sob demanda.
- Usar prefix e diretórios de dados por aplicação; suporte a import/export e backup.
- Registrar capabilities, logs, métricas e estado de compatibilidade por app.
- Manter fallback OpenShell Live/Debian e rollback da imagem Native.

### 1.2 Fora do escopo inicial

- Drivers Windows `.sys`, anti-cheat em kernel, serviços de kernel NT e compatibilidade universal com jogos.
- DXVK/VKD3D-Proton, DirectX 12, suporte GPU dedicado e otimizações NTSync sem prova prévia.
- Reescrever Wine, executar processos Windows dentro do renderer CEF ou criar kernel NT.
- Executar automaticamente instaladores desconhecidos com privilégios elevados.

## 2. Arquitetura-alvo

```mermaid
flowchart TD
  FE["OpenShell UI web — Files / Apps / Dock"] --> API["Host API — invoke / events"]
  API --> AM["Unified App Manager — Rust"]
  AM --> E{ "Tipo de aplicação" }
  E -->|Web| CEF["CEF app/browser context"]
  E -->|ELF| LIN["Debian App Runner"]
  E -->|PE32 / PE32+| WIN["Wine Runner"]
  WIN --> PM["Wine Prefix Manager"]
  WIN --> WI["Wine + wineserver + Win32 DLLs"]
  LIN --> GB["Linux Graphics Bridge — Xpra/X11 inicial"]
  WI --> GB
  GB --> SR["OpenShell Surface Registry"]
  CEF --> SR
  SR --> DISP["Display runtime / presentation"]
  WIN --> ABI["Asterinas Linux ABI + userspace"]
  LIN --> ABI
  DISP --> ABI
  ABI --> K["Asterinas Rust kernel"]
```

**Princípio de coesão:** reconhecer PE e iniciar Wine são responsabilidades do App Manager/runner; tradução Win32 é responsabilidade do Wine; integração de janelas é responsabilidade da ponte gráfica; governança de janelas continua no frontend via Surface Registry. Não criar um segundo window manager Windows.

### 2.1 Responsabilidades e limites

| Componente | Responsabilidade | Não deve fazer |
|---|---|---|
| `AppRegistry` | Cadastro, associação de arquivos, ícones, comandos, tipo de runtime | Interpretar DLLs/PE ou executar comandos arbitrários |
| `AppManager` | Lifecycle e identidade da aplicação, capabilities, escolha de runner | Implementar lógica Wine internamente |
| `WineRunner` | Resolver runtime, iniciar `wine`, acompanhar PID, wineserver, exit, logs | Controlar layout de janelas |
| `WinePrefixManager` | Criar, migrar, fazer backup e limpar prefix por app | Tratar prefix como sandbox de segurança |
| `LinuxGraphicsBridge` | Associar janelas X11/Xpra e eventos à aplicação | Reescrever widgets do aplicativo |
| `SurfaceRegistry` | Geometria, foco, z-order, visibilidade, workspace | Transportar pixels pela API JS |
| `PermissionBroker` | Exigir consentimento e limitar filesystem/rede/dispositivos | Confiar apenas nas permissões do Wine |
| `Asterinas` | ABI Linux, processos, VFS, memória, sinais, IPC e drivers | Expor Win32 como syscalls de kernel |

## 3. Formato de execução e contrato

### 3.1 Detecção de executáveis

- Reconhecer magic `MZ`, localizar e validar o cabeçalho `PE\0\0`, machine e seções sem confiar na extensão `.exe`.
- `PE32` e `PE32+` são candidatos a Wine; não executar DLL isolada como aplicativo sem um entry point apropriado.
- Bloquear PE de arquitetura não suportada, binários malformados e caminhos fora das capabilities concedidas.
- Não usar detecção de extensão como evidência suficiente; mostrar a origem, assinatura quando houver e risco de execução.
- Para `.msi`, expor fluxo separado via `msiexec`, somente após a primeira fase de instaladores.

### 3.2 Contrato sugerido

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

O exemplo define o modelo lógico, não uma API Rust já existente. Em implementação real, `LaunchRequest` deve referir identificadores/capabilities, não aceitar caminhos arbitrários fornecidos diretamente pelo renderer; validações, quotas e autorização devem ocorrer no host.

### 3.3 Eventos de lifecycle

`app.started`, `app.window-attached`, `app.focus-changed`, `app.exited`, `app.crashed`, `app.permission-required`, `wine.prefix-created`, `wine.prefix-migration-failed`, `wine.compatibility-warning`. Todos carregam `app_id`, `instance_id`, geração e timestamp; IDs de janela são atribuídos pelo host e vinculados à identidade do processo.

## 4. Organização do código

Ajustar os diretórios depois de conferir os repositórios reais; esta árvore é um alvo proposto, não uma afirmação sobre os arquivos atuais.

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
    └── host/                  # Adaptador UI: Apps / Files / launch
```

## 5. Fases de implementação e critérios de aceite

### Fase W0 — Inventário e baseline (bloqueante)

**Ações**
1. Fixar commit de Asterinas, patches, rootfs, CEF, Xpra e versão Wine; usar Wine 11.0 estável como baseline inicial ou outra versão explicitamente fixada após comparação.
2. Inspecionar integração Linux existente: como o App Manager lança ELF, como Xpra mapeia janelas ao desktop, como foco/resize/close são propagados.
3. Definir fixtures reproducíveis e observar `wine --version`, dependências `ldd`, ambiente de display, filesystem e permissões.
4. Montar a mesma imagem userspace de Wine no Linux de controle e no guest Asterinas, sem mudar versão das bibliotecas entre os experimentos.

**Entrega:** `compatibility-matrix.md`, manifest de versões, runners e captura de baseline.  
**Gate W0:** execução de app Debian continua íntegra; ambiente Wine reproduzível no Linux de controle.

### Fase W1 — Wine em userspace, sem integração com desktop

**Ações**
1. Empacotar Wine, bibliotecas, fontes, dependências X11 e utilitários necessários no rootfs; registrar licença e checksum dos pacotes.
2. Criar prefix isolado em diretório persistente do usuário e executar `wineboot -i`, `winecfg` e `wine notepad.exe` sob X11/Xpra.
3. Capturar primeira falha em Asterinas: syscall, flag, errno, ioctl, IPC, mmap, futex, sinal ou dependência userspace ausente. Reproduzir com um caso mínimo.
4. Criar testes Linux controle vs Asterinas e abrir issue upstream apenas quando o bug estiver localizado.
5. Não desativar sandbox ou rodar Wine como root para contornar bloqueios.

**Comandos de diagnóstico (somente laboratório, paths de exemplo):**

```bash
export WINEPREFIX="$HOME/.local/share/openshell/wine/prefixes/smoke-notepad"
export WINEDEBUG=-all
wineboot -i
wine notepad.exe
wineserver -w
```

**Gate W1:** `wineboot` conclui, Notepad abre e fecha sem processos órfãos no Linux controle e no Asterinas; logs identificam versão e configuração. Se o guest falhar, fase permanece aberta com lacunas documentadas.

### Fase W2 — App Manager + associação de `.exe`

**Ações**
1. Adicionar `RuntimeKind::Wine` ao registro de apps e detecção PE no host.
2. Criar `WineRunner` como adaptador do App Manager; runner inicia somente sob demanda, sem daemon global obrigatório.
3. Associar `.exe` a ação **Abrir com Wine**, inicialmente explícita e sujeita a confirmação para executáveis não confiáveis.
4. Controlar process group, PID, wineserver, cancelamento, timeout, logs e cleanup. Não confundir fechamento de uma janela com conclusão de um instalador multiprocesso.
5. Expor estados `starting/running/failed/exited` e erros compreensíveis na UI do OpenShell.

**Gate W2:** clicar duas vezes em um `.exe` suportado abre a aplicação com ícone e identidade no App Manager; fechar o app limpa os processos previstos sem encerrar outras aplicações.

### Fase W3 — Janelas Wine incorporadas ao desktop

**Ações**
1. Reutilizar o caminho X11/Xpra de aplicações Debian e vincular janelas Wine à instância do App Manager; garantir vínculo por X11 metadata, PID/process tree e tokens da sessão, sem confiar apenas em títulos.
2. Publicar `surfaceId`, `appId`, `instanceId`, `generation`, retângulo, escala, foco, z-order e visibilidade no `SurfaceRegistry`.
3. Testar múltiplas janelas de um mesmo processo, diálogos modais, menus, file picker, clipboard, drag/drop, IME, resize, minimize e workspace.
4. Evitar transportar bitmap inteiro através de JS; a UI apenas organiza a janela, enquanto a ponte transporta a superfície.
5. Manter Xpra como caminho validado; Wayland/XWayland é experimento posterior, não dependência do MVP.

**Gate W3:** Notepad integrado ao dock e ao window manager do OpenShell, sem janelas órfãs e sem quebrar menus do desktop, apps Debian ou Browser CEF.

### Fase W4 — Prefix Manager, instalação e estado persistente

**Ações**
1. Prefix por aplicativo: `data/wine/prefixes/<app-id>`; persistir registry, drives e arquivos. Manter cache e temporários em localização separada.
2. Criar wizard de instalação `.exe`/`.msi` com origem, destino, prefix, componentes solicitados, progressão e log; processos de instalação podem abrir várias janelas.
3. Expor `winecfg`, versão Wine, dependências, arquitetura e export/import de prefix (com backups e compatibilidade de versão).
4. Mapear discos Windows apenas para diretórios explicitamente permitidos. Pasta C: virtual não equivale ao filesystem inteiro do usuário.
5. Registrar apps instalados e atalhos; não confiar cegamente em entradas de registro nem nomes de executáveis.
6. Em upgrades A/B, migrar prefix com backup e versão gravada; permitir rollback sem corromper perfil persistente.

**Gate W4:** instalar e reiniciar uma aplicação de teste, conservar configurações, desinstalar e restaurar backup de prefix; confirmar que o rollback da imagem não danifica os dados.

### Fase W5 — Compatibilidade e performance avançadas

**Ações**
1. Testar primeiro Win32 64-bit, depois 32-bit via WoW64, aplicações .NET e bibliotecas adicionais, com fixtures separadas.
2. Avaliar Wine Mono, fontes e VC runtimes como dependências sob demanda, com licenças e origens verificadas.
3. Medir lançamento, RAM e latência em apps com múltiplos processos; separar custo Wine, Xpra, CEF e kernel.
4. Avaliar Wine Wayland driver e compositor mais direto quando existir suporte real no guest; não trocar Xpra por hipótese.
5. Considerar NTSync apenas se um aplicativo real demonstrar gargalo relevante. Asterinas não herda automaticamente o módulo Linux; interface equivalente exigiria desenvolvimento e validação específicos.
6. DXVK/VKD3D e jogos ficam condicionados à evolução de Vulkan/GPU e ao suporte do userspace.

**Gate W5:** matriz de compatibilidade publicada, regressões monitoradas e otimizações adotadas somente após benchmark comparável.

## 6. Segurança e políticas de execução

**Modelo de ameaça:** `.exe` executável é código não confiável. Wine converte chamadas do app em operações do userspace e não cria isolamento por si só. Prefix isolado previne conflito de configurações; **não é uma sandbox**.

Controles obrigatórios:
- Executar como usuário não privilegiado, nunca como root.
- Permissões por app/capability, com consentimento para diretórios adicionais, rede e integração de clipboard quando aplicável.
- Restringir montagem de volumes e symlinks; impedir que o mapeamento `Z:` exponha todo o filesystem sem consentimento.
- Isolamento adicional por namespaces, credenciais e filtros somente onde houver suporte real no Asterinas e testes de fronteira; declarar explicitamente quando uma proteção estiver indisponível.
- Não permitir chamadas diretas de sites web ao `WineRunner`; acesso somente pelo App Manager autenticado e autorizado.
- Limites de processo, memória, I/O e recursos, quando o kernel disponibilizar mecanismos adequados; fallback controlado, não simulacro de isolamento.
- Instalação de dependências com fontes verificadas; logs devem redigir tokens, diretórios privados sensíveis e outras credenciais.
- Diferenciar confiança do `.exe` de sua assinatura: assinatura não garante segurança.

**Bloqueio de lançamento:** se não houver isolamento mínimo para o perfil de risco de um aplicativo, o OpenShell deve negar a execução ou comunicar claramente o ambiente experimental; nunca conceder permissões silenciosamente.

## 7. Compatibilidade Linux/Asterinas — checklist de diagnóstico

Verificar no Linux controle e no Asterinas (sem presumir suporte):

| Área | Exemplos de testes | Sintoma |
|---|---|---|
| Processos | clone/fork/exec, wait, signals, process groups | Wine não inicia ou processos órfãos |
| Memória | mmap/mprotect, TLS, shared memory | crash em loader/DLL |
| Sincronização | futex, waits, timers, eventfd | deadlock/CPU alto |
| IPC | Unix sockets, pipes, wineserver | programas travam na inicialização |
| Filesystem | case sensitivity, symlinks, rename, locking, fsync | prefix/instalador danificado |
| Desktop | X11 sockets, clipboard, IME, dialogs | janela invisível ou input incorreto |
| Rede | TCP/UDP, DNS, TLS, proxy | aplicações sem conectividade |
| Áudio | backend disponível no userspace | reprodução/gravação indisponíveis |
| GPU | OpenGL, Vulkan, Mesa, DRM | fallback software / tela preta |
| Isolamento | user/pid/mount namespaces e filtros suportados | app sem fronteira de segurança |

Registrar a **primeira falha reproduzível**, versão kernel/Wine, host/guest, syscall, retorno, trace e fixture. Abrir alterações no kernel apenas quando o teste demonstrar que o problema está realmente na ABI e não no rootfs, Wine ou bridge gráfica.

## 8. Matriz de testes de aceitação

| ID | Cenário | Critério |
|---|---|---|
| WIN-001 | Inicializar prefix | `wineboot` sem erro e sem acesso fora das permissões |
| WIN-002 | Executar Wine Notepad | Janela visível e funcional; fechamento limpo |
| WIN-003 | Abrir `.exe` via Files | PE detectado; seleção Wine; identidade no dock |
| WIN-004 | Foco e resize | Input, z-index, escala e resize corretos |
| WIN-005 | Diálogos e múltiplas janelas | Filhos/modais vinculados à instância original |
| WIN-006 | Múltiplos apps | Dois prefixes isolados e processos independentes |
| WIN-007 | Reabrir sessão | Estado preservado; sem processos órfãos |
| WIN-008 | App não confiável | Nenhuma permissão extra sem autorização |
| WIN-009 | Apps Debian | GTK/Qt continuam funcionando no mesmo desktop |
| WIN-010 | Browser CEF | Cookies e sandbox de páginas não expõem Wine/host |
| WIN-011 | Upgrade e rollback | Prefix íntegro ao retornar versão anterior |
| WIN-012 | Falha Wine/Xpra | Erro diagnosticável sem derrubar OpenShell UI |
| WIN-013 | WoW64 | App 32-bit de teste sob runtime compatível |
| WIN-014 | Performance | Diferenças medidas contra Linux controle |

**Critério de liberação experimental:** WIN-001 a WIN-012, exceto WIN-011 que só se torna obrigatório quando a instalação A/B estiver habilitada. WIN-013 e WIN-014 são gates para ampliar compatibilidade, não para a primeira prova técnica.

## 9. Observabilidade e medição

Cada execução deve gerar `run_id`, `app_id`, `instance_id`, versão Wine, hash do `.exe` (sem compartilhar binário), kernel build ID, versão do rootfs, backend gráfico, prefix ID, tempos `request → wine spawned → first window → interactive`, RAM, CPU e código de saída.

Métricas prioritárias: startup frio/quente, RAM adicional em repouso, processos persistentes, tempo para primeira janela interativa, latência de entrada e resize, FPS quando relevante, falhas de render, operações de disco e estabilidade em múltiplas sessões. Comparar Wine no mesmo Linux de controle com e sem a integração OpenShell; depois comparar com Asterinas usando as mesmas fixtures.

Não estabelecer metas numéricas arbitrárias antes de medir baseline. Registrar p50/p95 e regressões por versão.

## 10. Riscos e mitigação

| Risco | Impacto | Mitigação |
|---|---|---|
| Falta de syscall/semântica em Asterinas | Alto | repro mínima, patch direcionado, upstream |
| Wine inicia mas não mostra janela | Alto | validar X11/Xpra isolado antes da UI |
| Wine exige bibliotecas adicionais | Médio | rootfs de compatibilidade com dependências fixadas |
| Executável Windows acessa arquivos privados | Crítico | capabilities, usuário limitado, isolamento comprovado |
| Prefix corrompido após atualização | Alto | backup, migração versionada e rollback |
| Performance ruim de composição | Médio/alto | superfície nativa, medir cópias, evitar JS pixels |
| Instalações multiprocesso | Médio | lifecycle por árvore de processos e wineserver |
| GPU/Vulkan insuficiente | Alto para jogos | não prometer jogos no MVP; fallback software |
| Atualizações Wine quebram apps | Médio | runtime versionado e compatibilidade por prefix |

## 11. Plano de entrega e dependências

| Prioridade | Entrega | Depende de | Resultado |
|---|---|---|---|
| P0 | W0 baseline | acesso aos builds e smoke atuais | ambiente reproduzível |
| P0 | W1 `wineboot`/Notepad | X11/Xpra, Linux ABI | primeira janela Windows |
| P1 | W2 launcher PE | App Manager / host API | abrir `.exe` no Files |
| P1 | W3 integração gráfica | LinuxGraphicsBridge / Surface Registry | janela Windows gerenciada pelo OpenShell |
| P1 | Segurança mínima auditável | permissões / isolamento real | lançamento controlado |
| P2 | W4 instaladores/prefixes | persistência e A/B | aplicações instaláveis |
| P3 | W5 WoW64/.NET/DXVK | compatibilidade e drivers | ecossistema expandido |

### Política de fallback

- Feature flag `native.wine.enabled=false` por padrão até WIN-001..012 exigidos para release experimental.
- Em erro do Wine, preservar sessão CEF/host; mostrar diagnóstico e opção de tentar novamente.
- Em falha do backend gráfico, manter launcher Linux e Wine desabilitados, sem comprometer as aplicações web.
- Não reverter prefix automaticamente sem snapshot/backup; informar migração incompatível.
- Manter OpenShell Live/Debian como referência e caminho de retorno.

## 12. Definition of Done para o primeiro marco integrado

- [ ] Mesma versão Wine inicia no Linux controle e no Asterinas.
- [ ] `wineboot` passa em prefix isolado persistente.
- [ ] Notepad do Wine é renderizado via X11/Xpra na janela OpenShell.
- [ ] Dock, focus, resize, minimizar, maximizar e fechar funcionam.
- [ ] Não há processos Wine órfãos após o encerramento esperado da aplicação/sessão.
- [ ] Abertura de `.exe` pelo Files usa o App Manager e pede autorização.
- [ ] Aplicações Debian e Browser CEF passam novamente pelos seus smokes.
- [ ] Dados privados não são expostos implicitamente via drives Wine.
- [ ] Logs e relatórios guardam Wine/kernel/rootfs/CEF e falhas reproduzíveis.
- [ ] Release pode desabilitar Wine por configuração, sem alterar o fluxo Linux/web.

## 13. ADRs propostos

- **WIN-ADR-01 — Wine em userspace opcional:** aceito como direção; não incluir no kernel.
- **WIN-ADR-02 — Unified App Manager:** estender runner existente, sem gerenciador paralelo Windows.
- **WIN-ADR-03 — Xpra/X11 primeiro:** adotado como caminho de prova; Wayland é evolutivo.
- **WIN-ADR-04 — Prefix por app:** isolamento de configuração obrigatório, não substitui sandbox.
- **WIN-ADR-05 — Wine versionado:** manifest por release, upgrades testados e rollback de dados.
- **WIN-ADR-06 — NTSync/DXVK condicionais:** não entram no MVP sem evidência de necessidade e suporte.

## 14. Fontes e notas de verificação

**Documentos do projeto fornecidos:** `OPENSHELL_NATIVE_ARCHITECTURE_FINAL.md` (12/11/2026) e *OpenShell Native — Migração do Electron para CEF* (11/11/2026). O segundo descreve shell Rust/CEF, X11/Openbox, `hostd`, Browser overlay, Xpra e smoke tests. Este plano não afirma ter executado ou auditado esses testes.

**Referências externas:**

- WineHQ, *Wine 11.0 Released* (13/01/2026): https://www.winehq.org/news/2026011301 — versão estável, WoW64 e NTSync.
- WineHQ, notícias e lançamentos: https://www.winehq.org/news/ — validar patches e versões antes de congelar runtime de produção.
- Wine source/documentação: https://gitlab.winehq.org/wine/wine/-/wikis/Documentation
- Asterinas: https://asterinas.github.io/book/ — compatibilidade Linux e limitações específicas da revisão usada.

**Condição para execução do plano:** validar o estado real do repositório e os testes do guest antes de aplicar paths, patches, APIs ou estimativas. A árvore proposta e as interfaces são desenhos de implementação, não código já integrado.

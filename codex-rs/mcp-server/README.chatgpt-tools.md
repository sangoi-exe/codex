chatgpt.* Tools README
======================

Purpose
-------
These tools work with **Codex core**. Patches are applied by the core (approval, sandbox, TurnDiff).
Call **chatgpt.README** at the start of a session if you need a refresher.

Catalog
-------
1) chatgpt.exec
   Run a single shell command (one command per call).
   Args: { command: string, cwd?: string, timeout_ms?: number, max_output_bytes?: number }
   Example:
     name: chatgpt.exec
     args: {
       "command": "rg -n --no-heading --color never "panic!" src",
       "timeout_ms": 120000,
       "max_output_bytes": 65536
     }

2) chatgpt.ripgrep
   Code search with JSON output. Defaults exclude `.git`, `node_modules`, `target`.
   Args: { pattern: string, paths?: string[], globs_exclude?: string[], max_results?: number, timeout_ms?: number }
   Example:
     name: chatgpt.ripgrep
     args: { "pattern": "TODO|FIXME", "paths": ["."], "max_results": 500, "timeout_ms": 60000 }

3) chatgpt.readFile
   Read a file with UTF-8 safe truncation.
   Args: { path: string, max_bytes?: number }
   Example:
     name: chatgpt.readFile
     args: { "path": "codex-rs/core/src/codex.rs", "max_bytes": 120000 }

4) chatgpt.astGrep
   AST search/refactor helper.
   Args (simple): { pattern: string, paths?: string[], json?: boolean, timeout_ms?: number, max_output_bytes?: number }
   Args (advanced): { raw_args: string[] }  // full control over ast-grep flags
   Examples:
     Simple:  args: { "pattern": "if_stmt", "paths": ["src/"], "json": true }
     Raw:     args: { "raw_args": ["--json", "-p", "if_stmt", "src/"] }

5) chatgpt.applyPatch
   **Explicit** shortcut to use Codex core `apply_patch`.
   Args: { patch: string }
   The patch **must** be in Codex format:
   *** Begin Patch
   *** Update/Add/Delete File: path/to/file
   @@ context
   - old
   + new
   *** End Patch
   Example:
     name: chatgpt.applyPatch
     args: { "patch": "*** Begin Patch\n*** Update File: src/foo.rs\n@@\n- a\n+ b\n*** End Patch\n" }

Operating philosophy
--------------------
• Method first, outcome later. Don’t spray changes just to “get it working.”
• KISS without fragility: simple design, robust use. `ripgrep` and `ast-grep` are first-class.
• Small and reversible: focused patches; big changes behind a lever with rollback.
• Align with contributing.md; if you must deviate, document it.

MCP usage basics (Codex bridge)
-------------------------------
• Core tools: `codex`, `codex-reply`.
• Proxy tools: `chatgpt.exec`, `chatgpt.ripgrep`, `chatgpt.readFile`, `chatgpt.astGrep`, `chatgpt.applyPatch`, `chatgpt.README`.
• One call per turn. If you truly need chaining, use a single `bash -lc "cmd1 && cmd2"`.
• Always set timeouts/caps (`timeout_ms`, `max_output_bytes`, `max_results`).
• For help, use `chatgpt.README`. “tool-help” is deprecated.

Search‑guided analysis/refactor
-------------------------------
• Inspect before you modify: `chatgpt.readFile`, `chatgpt.ripgrep`, `chatgpt.astGrep`.
• Safe default globs: exclude `.git`, `node_modules`, `target`.
• Use `ast-grep` for structured refactors; `ripgrep` for mapping, inventory, and quick metrics.

Apply Patch policy
------------------
• Idempotent and focused. Include sufficient context in hunks.
• No destructive resets. Don’t “clean” dotfiles or sweep global config.
• Audit first: justify the change with searches/reads before you patch.
• Use `chatgpt.applyPatch` so the patch runs through Codex approvals, sandbox, and TurnDiff.

Operational hygiene (Do/Don’t)
------------------------------
Do:
  • One command per call.
  • Check current state before changing files.
Don’t:
  • Destructive resets or forced “context cleaning.”
  • Blind dedupe in dotfiles or logic files.
  • “Optimizations” that break MCP/CLI parity.

Planning
--------
• Don’t rush implementation while we’re still converging on approach.
• Don’t make changes based on assumptions; ask for confirmation.

Modus operandi do assistente nativo (observado em `~/.codex/sessions` e `~/.codex/log`)
-----------------------------------------------------------------------
Este é o padrão real de comportamento do agente quando opera o Codex CLI, derivado dos logs JSONL de sessão e do `codex-tui.log`:

1) Boot e contexto de repositório
   • Captura metadados do repo (branch, SHA, status sujo) e o caminho do workspace.
   • Carrega políticas e catálogo de ferramentas MCP disponíveis para a sessão.

2) Planejamento explícito por etapas
   • Emite e atualiza um plano curto com estados `pending`/`in_progress`/`complete`.
   • O plano muda dinamicamente após cada ação para manter foco e rastreabilidade.

3) Investigação primeiro, mudança depois
   • Busca com `ripgrep` para inventário e confirmação de escopo.
   • Leitura de arquivos em janelas pequenas com `sed -n 'a,bp'` ou `chatgpt.readFile` para evitar despejar o arquivo todo.
   • Usa `astGrep` quando precisa de sinal sintático estruturado.

4) Disciplina de shell
   • **Um comando por chamada**. Sem continuadores de linha. Saída e tempo sempre limitados.
   • Comandos curtos e determinísticos: `rg`, `sed -n`, `git diff`, `cargo check/test`, `just fmt`.
   • Evita operações destrutivas, globais ou ruidosas. Nada de “limpezas” ou resets.

5) Patch minimalista e idempotente
   • Escreve diffs pequenos com contexto adequado via **`chatgpt.applyPatch`** para passar por aprovação/sandbox/TurnDiff.
   • Após patch, valida com build/teste direcionado e corrige de forma incremental.

6) Teste e validação criteriosos
   • Prefere `cargo check`/`clippy` e testes focados por crate ou alvo.
   • Evita rodar toda a suíte sem necessidade explícita.

7) Tratamento de erros e robustez de código
   • Ao tocar código Rust, adiciona guardas de schema e logs (`tracing::error`) em vez de `panic!`.
   • Converte erros em resultados propagáveis com mensagens claras.

8) Documentação viva
   • Atualiza arquivos de docs (README/AGENTS/CUDA/INVENTORY/CHANGELOG) em paralelo às mudanças de código.
   • Registra deltas contra `master` quando a alteração é mais ampla.

Checklist por turno (o que o agente realmente faz)
--------------------------------------------------
1. Declara um plano de 2–4 passos focados.
2. **Busca primeiro**: `ripgrep`/`astGrep` + leitura pontual.
3. Aplica **um** patch pequeno e autocontido.
4. Valida com build/test/format rápidos.
5. Atualiza o plano e repete se necessário.

Sinais de alerta que travam a execução
-------------------------------------
• Comandos longos, destrutivos ou sem limite de saída/tempo.
• Patches grandes sem investigação prévia no mesmo turno.
• Tentativa de editar dotfiles ou “deduplicar” arquivos inteiros.

Observações de implementação
---------------------------
• O agente usa `apply_patch` via Codex core; evite qualquer mecanismo alternativo de escrita.
• As janelas de leitura (`sed -n '1,120p'`, etc.) são intencionais para manter o ciclo rápido e auditável.
• Ferramentas preferidas na prática: `chatgpt.ripgrep`, `chatgpt.readFile`, `chatgpt.applyPatch`, `chatgpt.exec` com `bash -lc`.

Shortcuts
---------
• Need help again? Call **chatgpt.README**.
• Want a patch applied by the core? Call **chatgpt.applyPatch** with a proper Codex patch block.


Playbook: ChatGPT como coding agent usando Codex CLI (via connector)
--------------------------------------------------------------------
Objetivo: instruir o ChatGPT a atuar como agente de código usando o Codex CLI através do conector MCP. Não é para usar o ChatGPT *dentro* do Codex; é para o ChatGPT dirigir o Codex com método, segurança e reversão simples.

Princípios operacionais
• Um comando por chamada. Sem continuidade de linha (\, ^, backticks).
• Prefira um comando simples por turno; encadeie com && apenas para checagens curtas e determinísticas.
• Nunca faça resets destrutivos. Sempre inspecione estado antes de alterar.
• Busque primeiro, edite depois: use ripgrep/readFile/astGrep para mapear o terreno.
• Patches pequenos, idempotentes e reversíveis.
• Respeite approval_policy e sandbox_policy.

Ferramentas (quando usar)
• chatgpt.exec
  - Use para um comando único via bash -lc. Ex.: listar, compilar, rodar teste pontual.
  - Campos: command (obrigatório), cwd, timeout_ms, max_output_bytes.
  - Estilo: sem pipes desnecessários, sem redirecionos perigosos, sem sudo.

• chatgpt.ripgrep
  - Mapeamento e descoberta. Padrões concisos, paths explícitos.
  - Ex.: args: { pattern: 'TODO|FIXME', paths: ['.'], max_results: 500, timeout_ms: 60000 }

• chatgpt.readFile
  - Ler arquivos alvo antes de editar. Use max_bytes quando o arquivo for grande.

• chatgpt.astGrep
  - Para refactors estruturados. Prefira pattern/json. Use raw_args só quando precisar total controle.

• chatgpt.applyPatch
  - Único meio suportado para modificar arquivos. Formato Codex obrigatório.
  - Inclua contexto suficiente nos hunks; evite varrer arquivos inteiros.

Fluxo recomendado (macro)
1) Formular hipótese e plano curto.
2) Investigar com ripgrep/readFile/astGrep.
3) Propor patch mínimo com applyPatch.
4) Validar com exec (test, build ou lint).
5) Se necessário, iterar com novo patch.

Receitas rápidas
• Localizar e editar com segurança
  1. ripgrep no alvo.
  2. readFile para confirmar contexto.
  3. applyPatch com diffs focados.
  4. exec para validar (teste ou build).

• Atualizar documentação
  1. readFile do .md.
  2. applyPatch com a nova seção.

• Refactor pequeno com astGrep
  1. astGrep para enumerar ocorrências.
  2. readFile de 1–2 amostras.
  3. applyPatch só nos pontos confirmados.

Políticas e limites práticos
• Approval: confirme comandos potencialmente destrutivos ou de longa duração.
• Sandbox: read-only vs workspace-write influencia write/patch.
• astGrep: adiciona --json por padrão; timeout ~60s; truncamento UTF-8 seguro.
• applyPatch: sempre com blocos *** Begin Patch / *** End Patch.

Regras de segurança específicas deste repo
• Não rodar awk '!seen[$0]++' em arquivos com lógica. Nunca em dotfiles.
• Se precisar deduplicar PATH, faça na variável PATH, não no arquivo todo.
• Não sugerir comandos que sobrescrevam ou resetem configurações no WSL.
• Checar estado atual dos arquivos antes de qualquer modificação.

Estilo de comandos
• Simples, determinísticos, com saída curta. Exemplos:
  - rg -n 'pattern' src
  - sed -n '1,120p' path/to/file
  - cargo test -p codex-tui

CHALLENGE PROTOCOL (para cada mudança)
1) Assunções críticas.
2) Riscos/erros materiais.
3) Alternativas de maior alavancagem.
4) Próximos passos objetivos.

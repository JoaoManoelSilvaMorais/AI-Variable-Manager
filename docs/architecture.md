# Arquitetura do Sistema

O **AI Variable Manager** adota uma arquitetura em camadas inspirada em **Clean / Hexagonal Architecture**, garantindo separação estrita entre regras de domínio puras, persistência local, motores de arquivo/XML e interface nativa Slint.

---

## Camadas (`src/`)

```text
src/
├── main.rs                 # Bootstrap Win32/GUI, logs locais e inicialização do AppController
├── lib.rs                  # Exportação pública da biblioteca e container AppServices
├── domain/                 # Entidades puras, diagnósticos, regras de negócio e traits (ports)
│   ├── project.rs          # Project, ProjectMode, PathStrategy, ThemePreference, ProjectSnapshot
│   ├── variable.rs         # Variable, VariableKind (Text / LinkedFile), validação NCName
│   ├── record.rs           # Record, RecordValue, MatchStatus, CellDiagnostic, GridWindow
│   ├── asset.rs            # Asset, ScanResult, MatchCandidate, PhotoDiagnostics, UnmatchedPool
│   ├── validation.rs       # AuditReport, ValidationCode (E01..E10, W01..W07), ValidationTarget
│   ├── xml_model.rs        # ParsedXmlTemplate, TemplateVariableTrait, XmlExportSpec
│   └── ports.rs            # Traits: ProjectRepository, SpreadsheetReader, PhotoScanner, ThumbnailCache, XmlEngine, PackageExporter
├── application/            # Casos de uso orquestrando domínio e adaptadores
│   ├── mod.rs              # AppServices (contêiner compartilhado via Arc)
│   ├── project_service.rs  # Ciclo de vida de projetos, colunas/variáveis e preferências
│   ├── data_service.rs     # Grade paginada, edição de células, colagem TSV/CSV, diagnósticos
│   ├── import_service.rs   # Listagem de abas, prévia e commit transacional (.xlsx / .csv)
│   ├── photo_service.rs    # Varredura, match automático, vínculo manual/lote, cache LRU
│   ├── validation_service.rs # Auditoria pré-exportação completa e alvo de destino
│   └── export_service.rs   # Inspeção/vínculo XML, prévia, empacotamento atômico e rewrite de prefixo
├── infrastructure/         # Adaptadores concretos de sistema de arquivos e formatos
│   ├── sqlite/             # SQLite bundled (`rusqlite`) + migrações + repositório transacional
│   ├── spreadsheet/        # `calamine` (.xlsx/.xlsm), `csv` + cp1252 (.csv/.tsv), parser de clipboard
│   ├── media/              # `walkdir` scanner, matcher (Jaro-Winkler + stems), cache LRU off-thread
│   ├── xml/                # `quick-xml` parser/splicer Illustrator e encoder RFC-3986 `file:///`
│   └── export/             # Staging temporário `.aivm_stage_*`, cópia streaming e manifest.json
├── presentation/           # Modelos de UI e binding reativo entre Slint e AppServices
│   ├── app_controller.rs   # Binding completo das 5 abas, modais de confirmação e timer de miniaturas
│   ├── grid_model.rs       # Janela paginada (50 linhas/página) e estado do assistente de importação
│   ├── photo_model.rs      # Diagnósticos visuais, candidatos e conversão RGBA -> `slint::Image`
│   └── dialogs.rs          # Diálogos nativos `rfd` e leitura nativa da área de transferência Windows
└── shared/                 # Tipos de erro user-friendly (`AppError`), normalização pt-BR e UTC time
```

---

## Interface Gráfica (`ui/`)

- `ui/app.slint`: Janela principal (`AppWindow`) com barra lateral fixa (`Sidebar`), barra superior com banner de mensagens e tema (`Toolbar`), rodapé de status (`StatusBar`) e modal de confirmação (`ConfirmDialog`).
- `ui/theme/tokens.slint`: Structs de transferência Rust↔Slint e tokens semânticos adaptáveis para tema Claro e Escuro (`ThemeTokens` integrado ao `Palette` Fluent).
- `ui/views/`:
  - `projects_view.slint`: Criação, abertura, renomeio inline e exclusão segura de projetos.
  - `data_grid_view.slint`: Tabela paginada editável, gerenciador de colunas/variáveis, colagem de blocos e assistente overlay de importação.
  - `photos_view.slint`: Varredura, painel de KPIs diagnósticos, visualização com miniaturas sob demanda, confirmação de sugestões/ambiguidades e pool para vínculo em lote.
  - `xml_export_view.slint`: Vínculo de XML-base do Illustrator, mapeamento de colunas, lista clicável da auditoria, exportação portátil e reescritor de prefixos.
  - `settings_view.slint`: Escolha de tema, métricas de cache LRU, caminho do banco SQLite e checklist rápido para o Illustrator.

---

## Princípios de Segurança de Dados

1. **Somente leitura na entrada**: Nenhum código em `infrastructure/spreadsheet`, `infrastructure/media` ou `infrastructure/xml::inspect_template` abre fontes de dados com flag de escrita.
2. **Transacionalidade SQLite**: Operações críticas em `SqliteProjectRepository` utilizam transações explícitas com *rollback* automático em erro e restrições `FOREIGN KEY ... ON DELETE CASCADE`.
3. **Publicação atômica na saída**: A exportação prepara tudo em um diretório temporário irmão (`TempDirBuilder` `.aivm_stage_*`), valida a estrutura XML e a integridade das imagens copiadas via streaming antes de substituir os alvos em `target_dir`.

# Formato XML do Adobe Illustrator e Pacote Portátil

Este documento descreve exatamente o formato XML gerado e validado pelo motor `IllustratorXmlEngine` (`src/infrastructure/xml/`).

---

## Estrutura de Saída (`dados.xml`)

Independentemente do modo escolhido, o arquivo `dados.xml` gerado obedece estritamente às regras:
- **Codificação**: UTF-8 **sem BOM**.
- **Quebras de linha**: **CRLF (`\r\n`)** em todo o documento.
- **Escape XML**: Todos os 5 caracteres reservados (`&` -> `&amp;`, `<` -> `&lt;`, `>` -> `&gt;`, `"` -> `&quot;`, `'` -> `&apos;`) são escapados em nomes de *Data Sets*, textos e caminhos.

### 1. Modo XML com Modelo (`ProjectMode::WithTemplate`)
O motor preserva **byte a byte**:
- Declaração XML e `<!DOCTYPE svg [ ... ]>` (exigindo presença das entidades padrão Adobe: `ns_graphs`, `ns_vars`, `ns_imrep`, `ns_custom`, `ns_flows`, `ns_extend`);
- Cabeçalhos XMP / C2PA (`<x:xmpmeta>`, `<c2pa:manifest>`) quando presentes no arquivo original do usuário;
- Declarações `<variables>` e `<variable>` (incluindo tipos avançados como `visibility` ou `graphdata`, que permanecem preservados sem alteração).

Apenas o corpo interno de `<v:sampleDataSets>` (ou `<sampleDataSets>`) é substituído pelos novos registros `<v:sampleDataSet dataSetName="...">`.

> **Compatibilidade com modelos legados**: Alguns modelos criados em versões antigas do Illustrator declaram uma variável de foto com `trait="textcontent"`, porém nas amostras reais armazenam o caminho direto `<foto>caminho</foto>` sem envoltório `<p>`. O `IllustratorXmlEngine` detecta automaticamente `sample_uses_p_wrapper == Some(false)` na inspeção e emite o valor da foto **sem `<p>`**.

### 2. Modo Direto (`ProjectMode::WithoutTemplate`)
Gera um envelope SVG + Adobe VariableSet padrão completo contendo:
- `<!DOCTYPE svg [...]>` com as 6 entidades Adobe necessárias;
- `<variableSet varSetName="binding1" locked="none">` e `<variables>` com `<variable>` tipadas (`trait="textcontent"` para `Text` e `trait="fileref"` para `LinkedFile`);
- Bloco `<v:sampleDataSets>` com um `<v:sampleDataSet dataSetName="...">` por registro, envolvendo células de texto com `<p>...</p>` e células de imagem sem `<p>`.

---

## Caminho das Imagens no XML

Todas as fotografias efetivamente vinculadas são copiadas via *streaming* para `<destino>\imagens\` com nomes determinísticos e sequenciais (`asset_001.jpg`, `asset_002.png`, ...), garantindo independência em relação à pasta original de fotos. Fotos escaneadas que não foram vinculadas a nenhum registro **não** são copiadas.

No `dados.xml`, cada imagem vinculada é sempre gravada com o **caminho absoluto nativo do Windows** (barras invertidas `\` e caracteres UTF-8 literais, sem `file:///` e sem *percent-encoding* `%20`/`%C3%A1`), por exemplo:

```xml
<foto>D:\Downloads\crachás teste\imagens\asset_015.jpg</foto>
```

---

## Arquivos do Pacote Exportado

| Arquivo / Pasta | Finalidade |
|---|---|
| `dados.xml` | Arquivo principal para carregar em *Janela > Variáveis* no Adobe Illustrator. |
| `imagens/` | Cópias das fotografias vinculadas (`asset_001.*`, `asset_002.*`, ...). |
| `manifest.json` | Metadados estruturados da exportação (ID/nome do projeto, timestamps UTC, diretório exportado e mapa `source_path -> exported_rel_path / exported_uri`). Usado pelo atualizador de caminhos. |
| `relatorio.csv` | Relatório tabular CRLF com `record_id`, `dataset_name`, `status` (`OK` / `AVISO`), imagens exportadas e eventuais avisos de auditoria. |

---

## Atualizador de Caminhos em Pasta Movida (`Atualizar dados.xml`)

Quando uma pasta exportada é movida ou copiada para outro diretório ou computador:
1. O usuário seleciona a pasta na aba **XML e Exportação > Atualizar Caminhos das Imagens em Pasta Movida**.
2. O motor verifica se todas as imagens referenciadas existem em `imagens/`, reescreve os caminhos em `dados.xml` de forma atômica (via arquivo temporário) para o novo caminho absoluto nativo do Windows (`<nova_pasta>\imagens\asset_XXX.ext`) e atualiza `manifest.json`. Também converte automaticamente pacotes antigos que ainda contenham `file:///` ou `imagens/...`.

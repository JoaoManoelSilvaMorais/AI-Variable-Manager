# Matriz de Compatibilidade e Validação Manual no Adobe Illustrator

## 1. Compatibilidade de Entrada e Saída

| Categoria | Suportado | Observações |
|---|---|---|
| Sistema Operacional | Windows 10 / 11 (64-bit) | GUI nativa Fluent; fallback automático de tema pelo sistema operacional. |
| Planilhas | `.xlsx`, `.xlsm`, `.csv`, `.tsv`, `.txt` | `.xlsx` preserva zeros à esquerda; CSV detecta `,`, `;`, `\t`, UTF-8 BOM e Windows-1252 (`cp1252`). |
| Fotografias | `.jpg`, `.jpeg`, `.png`, `.tif`, `.tiff` | Varredura recursiva sem seguir symlinks; validação de cabeçalho na vinculação manual. |
| XML Adobe Illustrator | XML Variable Libraries (`<variableSet>`, `<v:sampleDataSets>`) | Testado com modelos padrão Adobe, modelos com bloco `<c2pa:manifest>` e modelos legados onde `foto` usa `trait="textcontent"` sem `<p>`. |

### Códigos da Auditoria Pré-Exportação

| Código | Severidade | Descrição |
|---|---|---|
| `E01` | Bloqueante | Projeto sem colunas ou sem registros. |
| `E02` | Bloqueante | Identificador XML (`xml_name`) vazio, inválido como `NCName` ou duplicado. |
| `E03` | Bloqueante | Nome de conjunto (`dataSetName`) vazio ou duplicado entre registros. |
| `E04` | Bloqueante | Campo de texto marcado como **Obrigatório** está vazio. |
| `E05` | Bloqueante | Campo de fotografia **Obrigatório** está sem vínculo ou apenas com sugestão não confirmada. |
| `E06` | Bloqueante | Arquivo de fotografia vinculado não existe mais no disco. |
| `E07` | Bloqueante | Fotografia vinculada está em formato não suportado ou arquivo com 0 bytes. |
| `E08` | Bloqueante | Modo XML com Modelo selecionado sem XML-base válido importado. |
| `E09` | Bloqueante | Conflito de mapeamento ou variável esperada no XML-base sem coluna mapeada. |
| `E10` | Bloqueante | Vínculo em estado **Ambíguo** pendente de desempate humano. |
| `W01` | Aviso | Célula/fotografia opcional vazia ou sem vínculo. |
| `W02` | Aviso | Sugestão automática de fotografia aguardando confirmação manual (em coluna opcional). |
| `W03` | Aviso | Valor de texto repetido entre múltiplos registros. |
| `W04` | Aviso | Mesma fotografia vinculada a mais de um registro. |
| `W05` | Aviso | Fotos escaneadas na pasta sem uso em nenhum registro (não serão copiadas). |
| `W06` | Aviso | Pasta de destino já possui arquivos de exportação (exigirá confirmação de sobrescrita). |
| `W07` | Aviso | Variável avançada do XML-base (`visibility` / `graphdata`) ignorada e preservada intacta. |

---

## 2. Roteiro de Validação Manual no Adobe Illustrator (Critérios 14 e 15)

Para comprovar a compatibilidade real no ambiente gráfico da estação de trabalho com Adobe Illustrator instalado:

### Cenário A — Pacote Exportado com XML-base Real e Espaços/Acentos no Caminho
1. Crie um projeto no **Modo XML com Modelo do Illustrator** e carregue um XML real salvo pelo seu Illustrator (ex.: arquivo com variáveis `nome`, `funcao`, `foto` e eventual bloco C2PA).
2. Importe uma planilha `.xlsx` ou `.csv` contendo acentos em português (`Á`, `ã`, `ç`), quebras de linha e caracteres especiais (`&`, `<`, `>`, `"`, `'`), além de códigos com zero à esquerda (`00105`).
3. Escaneie uma pasta de fotos, execute **Match Automático**, confirme sugestões/ambiguidades e vincule ao menos 1 foto manualmente.
4. Na aba **XML e Exportação**, execute **Executar Auditoria** (0 erros bloqueantes) e exporte para uma pasta contendo espaços e acentos (ex.: `D:\Downloads\crachás teste`).
5. Confirme na prévia/arquivo `dados.xml` que os caminhos das fotos foram gerados como `D:\Downloads\crachás teste\imagens\asset_001.jpg` (caminho absoluto nativo Windows, sem `file:///` e sem `%20`).
6. Abra o documento `.ai` correspondente no Adobe Illustrator > **Janela > Variáveis**.
7. No menu do painel **Variáveis**, clique em **Carregar Biblioteca de Variáveis...** e selecione `D:\Downloads\crachás teste\dados.xml`.
8. **Verificação esperada**:
   - O Illustrator carrega todos os *Data Sets* sem erros de DTD, entidade ou malformação XML.
   - Acentuação, zeros à esquerda, `&`, `<`, `>`, aspas e múltiplas linhas renderizam corretamente no canvas.
   - As fotografias vinculadas exibem a imagem correta ao navegar entre os *Data Sets*.

### Cenário B — Pasta Exportada Movida para Outro Diretório
1. Copie ou mova toda a pasta exportada para outro local (ex.: `C:\Temp\Destino Movido\crachás teste`).
2. No **AI Variable Manager**, em **XML e Exportação > Atualizar Caminhos das Imagens em Pasta Movida**, selecione a pasta movida e clique em **Atualizar dados.xml**.
3. Carregue novamente o `dados.xml` atualizado no Illustrator e confirme que todos os vínculos de imagem continuam carregando no novo caminho absoluto.

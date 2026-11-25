# Guia de Desenvolvimento, Testes e Empacotamento

## Pré-requisitos no Windows

- **Rust** (`MSRV 1.88+`, edição 2024) + toolchain `x86_64-pc-windows-msvc`
- **Visual Studio Build Tools 2022** e **Windows 10/11 SDK** (`rc.exe` para embutir `assets/app.rc` e `assets/app.ico` no executável)
- **Inno Setup 6** (`ISCC.exe`) para gerar o instalador `.exe` a partir de `installer/ai-variable-manager.iss`

> **Nota**: Não utilize a feature `renderer-skia` sem LLVM/Clang configurado; o projeto já compila nativamente com `backend-winit`, `renderer-femtovg` e `renderer-software` em estilo **Fluent**.

---

## Comandos Úteis

### Executar o aplicativo em modo desenvolvimento
```powershell
cargo run
```

### Análise estática (Clippy)
```powershell
cargo clippy --all-targets --features e2e_tests
```

### Executar toda a suíte automatizada offline
```powershell
# Testes unitários internos (27)
cargo test --lib

# Persistência SQLite e migrações (15)
cargo test --test m1_persistence

# Compatibilidade XML com fixtures de referência (3 offline)
cargo test --test xml_compat_tests

# Suíte E2E completa Tiers 1 a 4 — 246 cenários incluindo volume 1.000 x 1.000
cargo test --test e2e_runner --features e2e_tests
```

### Validação opcional somente-leitura contra o compartilhamento LAN de referência
```powershell
cargo test --test xml_compat_tests real_share_xmls_read_only_compatibility -- --ignored --nocapture
```

---

## Build de Release e Instalador Windows

1. Gere o executável otimizado (subsystem Windows sem janela de console):
   ```powershell
   cargo build --release
   ```
   Saída esperada: `target\release\ai-variable-manager.exe`.

2. Compile o instalador Windows com Inno Setup 6:
   ```powershell
   & "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" installer\ai-variable-manager.iss
   # ou em instalação global do sistema:
   # & "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\ai-variable-manager.iss
   ```
   Saída esperada: `installer\output\AI_Variable_Manager_Setup_0.1.0_x64.exe`.

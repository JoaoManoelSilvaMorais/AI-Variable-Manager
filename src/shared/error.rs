use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error(transparent)]
    Infra(#[from] InfraError),

    #[error("Funcionalidade ainda não implementada: {0}")]
    NotImplemented(String),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("Erro de validação: {0}")]
    Validation(String),

    #[error("Conflito de dados: {0}")]
    Conflict(String),

    #[error("{entity} não encontrado(a): {id}")]
    NotFound { entity: &'static str, id: String },

    #[error("Estado inválido para esta operação: {0}")]
    InvalidState(String),

    #[error("Formato não suportado: {0}")]
    UnsupportedFormat(String),

    #[error("Exportação bloqueada: {blocking_count} erro(s) impeditivo(s) encontrado(s).")]
    PreExportBlocked { blocking_count: usize },

    #[error("A pasta de destino já contém arquivos de exportação e requer confirmação de sobrescrita: {target_dir}")]
    OverwriteConfirmationRequired {
        target_dir: String,
        existing_paths: Vec<String>,
    },
}

#[derive(Debug, Error)]
pub enum InfraError {
    #[error("Erro no banco de dados ({context}): {detail}")]
    Database { context: String, detail: String },

    #[error("Erro de entrada/saída ({context}): {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Erro ao processar planilha: {0}")]
    Spreadsheet(String),

    #[error("Erro no motor XML: {0}")]
    Xml(String),

    #[error("Erro ao processar mídia/fotografias: {0}")]
    Media(String),

    #[error("Erro ao serializar/desserializar dados: {0}")]
    Serialization(String),

    #[error("Erro ao exportar pacote portátil: {0}")]
    Export(String),

    #[error("Erro de plataforma/sistema: {0}")]
    Platform(String),
}

impl AppError {
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Domain(DomainError::Validation(msg.into()))
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Domain(DomainError::Conflict(msg.into()))
    }

    pub fn not_found(entity: &'static str, id: impl Into<String>) -> Self {
        Self::Domain(DomainError::NotFound {
            entity,
            id: id.into(),
        })
    }

    pub fn invalid_state(msg: impl Into<String>) -> Self {
        Self::Domain(DomainError::InvalidState(msg.into()))
    }

    pub fn unsupported_format(msg: impl Into<String>) -> Self {
        Self::Domain(DomainError::UnsupportedFormat(msg.into()))
    }

    pub fn not_implemented(feature: impl Into<String>) -> Self {
        Self::NotImplemented(feature.into())
    }

    pub fn database(context: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Infra(InfraError::Database {
            context: context.into(),
            detail: detail.into(),
        })
    }

    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Infra(InfraError::Io {
            context: context.into(),
            source,
        })
    }

    pub fn spreadsheet(msg: impl Into<String>) -> Self {
        Self::Infra(InfraError::Spreadsheet(msg.into()))
    }

    pub fn xml(msg: impl Into<String>) -> Self {
        Self::Infra(InfraError::Xml(msg.into()))
    }

    pub fn media(msg: impl Into<String>) -> Self {
        Self::Infra(InfraError::Media(msg.into()))
    }

    pub fn serialization(msg: impl Into<String>) -> Self {
        Self::Infra(InfraError::Serialization(msg.into()))
    }

    pub fn export(msg: impl Into<String>) -> Self {
        Self::Infra(InfraError::Export(msg.into()))
    }

    pub fn platform(msg: impl Into<String>) -> Self {
        Self::Infra(InfraError::Platform(msg.into()))
    }

    pub fn user_message(&self) -> String {
        match self {
            Self::Domain(err) => err.to_string(),
            Self::NotImplemented(feature) => {
                format!("Funcionalidade ainda não disponível nesta etapa: {feature}")
            }
            Self::Infra(infra) => match infra {
                InfraError::Database { context, .. } => {
                    format!("Falha ao gravar ou ler os dados locais do projeto ({context}).")
                }
                InfraError::Io { context, .. } => {
                    format!(
                        "Não foi possível acessar o arquivo ou pasta ({context}). Verifique se o caminho existe e se há permissão de leitura/escrita."
                    )
                }
                InfraError::Spreadsheet(msg) => format!("Não foi possível ler a planilha: {msg}"),
                InfraError::Xml(msg) => format!("Erro ao processar o arquivo XML: {msg}"),
                InfraError::Media(msg) => format!("Erro ao processar imagem: {msg}"),
                InfraError::Serialization(msg) => format!("Erro ao formatar dados internos: {msg}"),
                InfraError::Export(msg) => format!("Falha ao exportar pacote: {msg}"),
                InfraError::Platform(msg) => format!("Erro de sistema operacional: {msg}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_hides_raw_sqlite_and_io_internals() {
        let db_err = AppError::database("abrir projeto", "SQLITE_BUSY: database is locked");
        let msg = db_err.user_message();
        assert!(msg.contains("abrir projeto"));
        assert!(!msg.contains("SQLITE_BUSY"));

        let io_err = AppError::io(
            "ler pasta de fotos",
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "os error 5"),
        );
        let io_msg = io_err.user_message();
        assert!(io_msg.contains("ler pasta de fotos"));
        assert!(!io_msg.contains("os error 5"));
    }

    #[test]
    fn domain_error_user_message_preserves_context() {
        let err = AppError::not_found("Projeto", "proj-1");
        assert!(err.user_message().contains("proj-1"));
    }
}

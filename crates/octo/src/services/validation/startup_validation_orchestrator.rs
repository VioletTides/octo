use std::sync::Arc;

use octo_core::validation::IStartupValidator;
use tokio_util::sync::CancellationToken;

/// Orchestrates startup validation for all configured services. The checks finish before the
/// server starts listening: [`crate::host::run`] awaits [`StartupValidationOrchestrator::start`]
/// before it binds.
pub struct StartupValidationOrchestrator {
    validators: Vec<Arc<dyn IStartupValidator>>,
}

impl StartupValidationOrchestrator {
    pub fn new(validators: Vec<Arc<dyn IStartupValidator>>) -> Self {
        StartupValidationOrchestrator { validators }
    }

    pub async fn start(&self) {
        println!();
        println!("========================================");
        println!("       Octo starting up...       ");
        println!("========================================");
        println!();

        // Run all validators. Each one catches its own failures and turns them into a result,
        // so nothing escapes here.
        let token = CancellationToken::new();
        for validator in &self.validators {
            validator.validate(&token).await;
        }

        println!();
        println!("========================================");
        println!("       Startup validation complete      ");
        println!("========================================");
        println!();
    }
}

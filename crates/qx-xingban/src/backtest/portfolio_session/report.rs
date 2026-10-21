use super::PortfolioBarSessionReport;
use qx_core::ReplayVerifier;

impl PortfolioBarSessionReport {
    pub fn result_hash(&self) -> u64 {
        self.event_log.digest()
    }

    pub fn verify(&self) -> Result<(), qx_core::QxError> {
        self.event_log.validate()?;
        ReplayVerifier::verify(self.event_log.events(), &self.ledger)?;
        Ok(())
    }
}

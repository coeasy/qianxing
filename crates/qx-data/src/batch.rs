//! Deterministic batch loading over provider contracts.

use crate::pipeline::process_bars;
use crate::provider::DataProvider;
use crate::schema::Bar;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarRequest {
    pub instrument: String,
    pub start: u64,
    pub end: u64,
}

impl BarRequest {
    pub fn new(instrument: impl Into<String>, start: u64, end: u64) -> Result<Self, String> {
        let request = Self {
            instrument: instrument.into(),
            start,
            end,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.instrument.trim().is_empty() {
            return Err("bar request instrument is required".into());
        }
        if self.start == 0 || self.start > self.end {
            return Err("bar request time range is invalid".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct BarBatchItem {
    pub request: BarRequest,
    pub bars: Vec<Bar>,
}

pub fn load_bar_batch<P: DataProvider>(
    provider: &P,
    requests: &[BarRequest],
) -> Result<Vec<BarBatchItem>, String> {
    let mut output = Vec::with_capacity(requests.len());
    for request in requests {
        request.validate()?;
        let raw = provider.load_bars(&request.instrument, request.start, request.end)?;
        if raw.iter().any(|bar| {
            bar.instrument != request.instrument
                || bar.timestamp < request.start
                || bar.timestamp > request.end
        }) {
            return Err(format!(
                "provider returned rows outside requested identity/range: {}",
                request.instrument
            ));
        }
        let (bars, _) = process_bars(raw)?;
        output.push(BarBatchItem {
            request: request.clone(),
            bars,
        });
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderMetadata;

    struct TestProvider;

    impl DataProvider for TestProvider {
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                name: "test".into(),
                version: "v1".into(),
            }
        }

        fn load_bars(&self, instrument: &str, start: u64, end: u64) -> Result<Vec<Bar>, String> {
            Ok((start..=end)
                .rev()
                .map(|timestamp| Bar {
                    instrument: instrument.into(),
                    timestamp,
                    open_raw: 10,
                    high_raw: 12,
                    low_raw: 9,
                    close_raw: 11,
                    volume_raw: 100,
                })
                .collect())
        }
    }

    #[test]
    fn batch_loader_canonicalizes_each_request() {
        let requests = vec![BarRequest::new("XSHG:600000", 1, 2).unwrap()];
        let batch = load_bar_batch(&TestProvider, &requests).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].bars[0].timestamp, 1);
        assert_eq!(batch[0].bars[1].timestamp, 2);
    }
}

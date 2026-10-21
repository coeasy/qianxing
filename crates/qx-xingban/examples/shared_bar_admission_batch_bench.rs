use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::hint::black_box;
use std::time::Instant;

#[derive(Clone)]
struct Market {
    priority: u32,
    instrument: String,
}

#[derive(Clone)]
struct InputOrder {
    market_index: usize,
    visible_ts: u64,
    order_id: u64,
}

#[derive(Debug, PartialEq, Eq)]
struct Batch {
    market_index: usize,
    visible_ts: u64,
    order_ids: Vec<u64>,
}

type Grouping = fn(&[InputOrder], &[Market]) -> Vec<Batch>;

fn batch_order(left: &Batch, right: &Batch, markets: &[Market]) -> std::cmp::Ordering {
    let left_market = &markets[left.market_index];
    let right_market = &markets[right.market_index];
    left_market
        .priority
        .cmp(&right_market.priority)
        .then_with(|| left_market.instrument.cmp(&right_market.instrument))
        .then_with(|| left.visible_ts.cmp(&right.visible_ts))
}

fn btree_group(orders: &[InputOrder], markets: &[Market]) -> Vec<Batch> {
    let mut grouped = BTreeMap::<(usize, u64), Vec<u64>>::new();
    for order in orders {
        grouped
            .entry((order.market_index, order.visible_ts))
            .or_default()
            .push(order.order_id);
    }
    let mut batches = grouped
        .into_iter()
        .map(|((market_index, visible_ts), order_ids)| Batch {
            market_index,
            visible_ts,
            order_ids,
        })
        .collect::<Vec<_>>();
    batches.sort_by(|left, right| batch_order(left, right, markets));
    batches
}

fn vec_hash_index_group(orders: &[InputOrder], markets: &[Market]) -> Vec<Batch> {
    let mut batches = Vec::<Batch>::new();
    let mut batch_indexes = HashMap::<(usize, u64), usize>::new();
    for order in orders {
        let key = (order.market_index, order.visible_ts);
        let batch_index = *batch_indexes.entry(key).or_insert_with(|| {
            batches.push(Batch {
                market_index: order.market_index,
                visible_ts: order.visible_ts,
                order_ids: Vec::new(),
            });
            batches.len() - 1
        });
        batches[batch_index].order_ids.push(order.order_id);
    }
    batches.sort_by(|left, right| batch_order(left, right, markets));
    batches
}

fn measure_vec_hash_reused(input: &[InputOrder], markets: &[Market], iterations: usize) -> u128 {
    let mut batches = Vec::<Batch>::new();
    let mut batch_indexes = HashMap::<(usize, u64), usize>::new();
    let start = Instant::now();
    for _ in 0..iterations {
        batches.clear();
        batch_indexes.clear();
        for order in input {
            let key = (order.market_index, order.visible_ts);
            let batch_index = *batch_indexes.entry(key).or_insert_with(|| {
                batches.push(Batch {
                    market_index: order.market_index,
                    visible_ts: order.visible_ts,
                    order_ids: Vec::new(),
                });
                batches.len() - 1
            });
            batches[batch_index].order_ids.push(order.order_id);
        }
        batches.sort_by(|left, right| batch_order(left, right, markets));
        black_box(&batches);
    }
    start.elapsed().as_nanos() / iterations as u128
}

fn vec_market_index_group(orders: &[InputOrder], markets: &[Market]) -> Vec<Batch> {
    let mut batches = Vec::<Batch>::new();
    let mut batch_indexes = (0..markets.len())
        .map(|_| Vec::<(u64, usize)>::new())
        .collect::<Vec<_>>();
    for order in orders {
        let market_indexes = &mut batch_indexes[order.market_index];
        let batch_index = if let Some((_, batch_index)) = market_indexes
            .iter()
            .find(|(visible_ts, _)| *visible_ts == order.visible_ts)
        {
            *batch_index
        } else {
            batches.push(Batch {
                market_index: order.market_index,
                visible_ts: order.visible_ts,
                order_ids: Vec::new(),
            });
            let batch_index = batches.len() - 1;
            market_indexes.push((order.visible_ts, batch_index));
            batch_index
        };
        batches[batch_index].order_ids.push(order.order_id);
    }
    batches.sort_by(|left, right| batch_order(left, right, markets));
    batches
}

fn vec_primary_index_group(orders: &[InputOrder], markets: &[Market]) -> Vec<Batch> {
    let mut batches = Vec::<Batch>::with_capacity(markets.len());
    let mut primary_visible_ts = vec![None::<u64>; markets.len()];
    let mut primary_batch_indexes = vec![None::<usize>; markets.len()];
    let mut secondary_batch_indexes = HashMap::<(usize, u64), usize>::new();
    for order in orders {
        let market_index = order.market_index;
        let batch_index = match primary_visible_ts[market_index] {
            None => {
                let batch_index = batches.len();
                batches.push(Batch {
                    market_index,
                    visible_ts: order.visible_ts,
                    order_ids: Vec::new(),
                });
                primary_visible_ts[market_index] = Some(order.visible_ts);
                primary_batch_indexes[market_index] = Some(batch_index);
                batch_index
            }
            Some(visible_ts) if visible_ts == order.visible_ts => {
                primary_batch_indexes[market_index].expect("primary batch index was inserted")
            }
            Some(_) => {
                let key = (market_index, order.visible_ts);
                *secondary_batch_indexes.entry(key).or_insert_with(|| {
                    batches.push(Batch {
                        market_index,
                        visible_ts: order.visible_ts,
                        order_ids: Vec::new(),
                    });
                    batches.len() - 1
                })
            }
        };
        batches[batch_index].order_ids.push(order.order_id);
    }
    batches.sort_by(|left, right| batch_order(left, right, markets));
    batches
}

fn vec_aligned_dense_group(orders: &[InputOrder], markets: &[Market]) -> Vec<Batch> {
    let mut batches = Vec::<Batch>::new();
    let mut batch_by_market = vec![usize::MAX; markets.len()];
    for order in orders {
        let batch_index = if batch_by_market[order.market_index] == usize::MAX {
            batches.push(Batch {
                market_index: order.market_index,
                visible_ts: order.visible_ts,
                order_ids: Vec::new(),
            });
            let index = batches.len() - 1;
            batch_by_market[order.market_index] = index;
            index
        } else {
            batch_by_market[order.market_index]
        };
        batches[batch_index].order_ids.push(order.order_id);
    }
    batches.sort_by(|left, right| batch_order(left, right, markets));
    batches
}

fn measure_aligned_dense_reused(
    input: &[InputOrder],
    markets: &[Market],
    iterations: usize,
) -> u128 {
    assert!(input
        .iter()
        .all(|order| order.visible_ts == input[0].visible_ts));
    let mut batches = Vec::<Batch>::with_capacity(markets.len());
    let mut batch_by_market = vec![usize::MAX; markets.len()];
    let mut touched_markets = Vec::<usize>::with_capacity(markets.len());
    let start = Instant::now();
    for _ in 0..iterations {
        let mut active_batches = 0;
        for order in input {
            let batch_index = batch_by_market[order.market_index];
            let batch_index = if batch_index == usize::MAX {
                if active_batches == batches.len() {
                    batches.push(Batch {
                        market_index: order.market_index,
                        visible_ts: order.visible_ts,
                        order_ids: Vec::new(),
                    });
                }
                let batch = &mut batches[active_batches];
                batch.market_index = order.market_index;
                batch.visible_ts = order.visible_ts;
                batch.order_ids.clear();
                batch_by_market[order.market_index] = active_batches;
                touched_markets.push(order.market_index);
                active_batches += 1;
                active_batches - 1
            } else {
                batch_index
            };
            batches[batch_index].order_ids.push(order.order_id);
        }
        batches[..active_batches].sort_by(|left, right| batch_order(left, right, markets));
        black_box(&batches[..active_batches]);
        for market_index in touched_markets.drain(..) {
            batch_by_market[market_index] = usize::MAX;
        }
    }
    start.elapsed().as_nanos() / iterations as u128
}

fn median(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn measure(input: &[InputOrder], markets: &[Market], group: Grouping, iterations: usize) -> u128 {
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(group(black_box(input), black_box(markets)));
    }
    start.elapsed().as_nanos() / iterations as u128
}

fn main() {
    const REPEATS: usize = 9;
    let mut rows = Vec::new();
    for callbacks in [16_usize, 64, 256] {
        let markets = (0..callbacks)
            .map(|index| Market {
                priority: (index % 8) as u32,
                instrument: format!("MKT-{index:04}"),
            })
            .collect::<Vec<_>>();
        for orders_per_callback in [1_usize, 4, 16] {
            for distribution in ["concentrated", "spread"] {
                for timestamp_pattern in ["aligned", "staggered"] {
                    let input = (0..callbacks)
                        .flat_map(|source| {
                            (0..orders_per_callback).map(move |ordinal| {
                                let target = if distribution == "concentrated" {
                                    0
                                } else {
                                    (source + ordinal) % callbacks
                                };
                                InputOrder {
                                    market_index: target,
                                    visible_ts: if timestamp_pattern == "aligned" {
                                        1
                                    } else {
                                        1 + (source % 8) as u64
                                    },
                                    order_id: (source * orders_per_callback + ordinal) as u64,
                                }
                            })
                        })
                        .collect::<Vec<_>>();
                    let expected = btree_group(&input, &markets);
                    assert_eq!(expected, vec_hash_index_group(&input, &markets));
                    assert_eq!(expected, vec_market_index_group(&input, &markets));
                    assert_eq!(expected, vec_primary_index_group(&input, &markets));
                    if timestamp_pattern == "aligned" {
                        assert_eq!(expected, vec_aligned_dense_group(&input, &markets));
                    }

                    let iterations = (16_384 / input.len()).clamp(16, 16_384);
                    let mut btree_samples = Vec::with_capacity(REPEATS);
                    let mut hash_samples = Vec::with_capacity(REPEATS);
                    let mut market_index_samples = Vec::with_capacity(REPEATS);
                    let mut primary_index_samples = Vec::with_capacity(REPEATS);
                    let mut reused_hash_samples = Vec::with_capacity(REPEATS);
                    let mut dense_samples = Vec::with_capacity(REPEATS);
                    for repeat in 0..REPEATS {
                        if repeat % 2 == 0 {
                            btree_samples.push(measure(&input, &markets, btree_group, iterations));
                            hash_samples.push(measure(
                                &input,
                                &markets,
                                vec_hash_index_group,
                                iterations,
                            ));
                            market_index_samples.push(measure(
                                &input,
                                &markets,
                                vec_market_index_group,
                                iterations,
                            ));
                            primary_index_samples.push(measure(
                                &input,
                                &markets,
                                vec_primary_index_group,
                                iterations,
                            ));
                            reused_hash_samples
                                .push(measure_vec_hash_reused(&input, &markets, iterations));
                            if timestamp_pattern == "aligned" {
                                dense_samples.push(measure_aligned_dense_reused(
                                    &input, &markets, iterations,
                                ));
                            }
                        } else {
                            if timestamp_pattern == "aligned" {
                                dense_samples.push(measure_aligned_dense_reused(
                                    &input, &markets, iterations,
                                ));
                            }
                            reused_hash_samples
                                .push(measure_vec_hash_reused(&input, &markets, iterations));
                            primary_index_samples.push(measure(
                                &input,
                                &markets,
                                vec_primary_index_group,
                                iterations,
                            ));
                            market_index_samples.push(measure(
                                &input,
                                &markets,
                                vec_market_index_group,
                                iterations,
                            ));
                            hash_samples.push(measure(
                                &input,
                                &markets,
                                vec_hash_index_group,
                                iterations,
                            ));
                            btree_samples.push(measure(&input, &markets, btree_group, iterations));
                        }
                    }
                    let old_ns = median(&mut btree_samples);
                    let hash_ns = median(&mut hash_samples);
                    let market_index_ns = median(&mut market_index_samples);
                    let primary_index_ns = median(&mut primary_index_samples);
                    let reused_hash_ns = median(&mut reused_hash_samples);
                    let dense_ns = if dense_samples.is_empty() {
                        None
                    } else {
                        Some(median(&mut dense_samples))
                    };
                    rows.push(json!({
                    "benchmark": "shared_bar_admission_batch",
                    "callbacks": callbacks,
                    "orders_per_callback": orders_per_callback,
                    "distribution": distribution,
                    "timestamp_pattern": timestamp_pattern,
                    "orders": input.len(),
                    "unique_batches": expected.len(),
                    "repeats": REPEATS,
                    "iterations_per_sample": iterations,
                    "outputs_equal": true,
                    "btree_median_ns_per_operation": old_ns,
                    "vec_hash_index_median_ns_per_operation": hash_ns,
                    "vec_market_index_median_ns_per_operation": market_index_ns,
                    "vec_primary_index_median_ns_per_operation": primary_index_ns,
                    "vec_reused_hash_median_ns_per_operation": reused_hash_ns,
                    "vec_aligned_dense_reused_median_ns_per_operation": dense_ns,
                    "vec_hash_speedup_ratio": old_ns as f64 / hash_ns.max(1) as f64,
                    "vec_market_index_speedup_ratio": old_ns as f64 / market_index_ns.max(1) as f64,
                    "vec_primary_index_speedup_ratio": old_ns as f64 / primary_index_ns.max(1) as f64,
                    "vec_reused_hash_speedup_ratio": old_ns as f64 / reused_hash_ns.max(1) as f64,
                    "vec_aligned_dense_reused_speedup_ratio": dense_ns.map(|value| old_ns as f64 / value.max(1) as f64),
                }));
                }
            }
        }
    }
    for row in rows {
        println!("{}", row);
    }
}

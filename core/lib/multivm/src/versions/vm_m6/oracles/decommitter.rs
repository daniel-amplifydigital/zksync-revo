use std::collections::HashMap;

use zk_evm_1_3_1::{
    abstractions::{DecommittmentProcessor, Memory, MemoryType},
    aux_structures::{
        DecommittmentQuery, MemoryIndex, MemoryLocation, MemoryPage, MemoryQuery, Timestamp,
    },
};
use zksync_types::{u256_to_h256, U256};

use super::OracleWithHistory;
use crate::{
    utils::bytecode::{bytecode_len_in_words, bytes_to_be_words},
    vm_m6::{
        history_recorder::{HistoryEnabled, HistoryMode, HistoryRecorder, WithHistory},
        storage::{Storage, StoragePtr},
    },
};

/// The main job of the DecommiterOracle is to implement the DecommitmentProcessor trait - that is
/// used by the VM to 'load' bytecodes into memory.
#[derive(Debug)]
pub struct DecommitterOracle<const B: bool, S: Storage, H: HistoryMode> {
    /// Pointer that enables to read contract bytecodes from the database.
    storage: StoragePtr<S>,
    /// The cache of bytecodes that the bootloader "knows", but that are not necessarily in the database.
    /// And it is also used as a database cache.
    pub known_bytecodes: HistoryRecorder<HashMap<U256, Vec<U256>>, H>,
    /// Stores pages of memory where certain code hashes have already been decommitted.
    /// It is expected that they all are present in the DB.
    // `decommitted_code_hashes` history is necessary
    pub decommitted_code_hashes: HistoryRecorder<HashMap<U256, u32>, HistoryEnabled>,
    /// Stores history of decommitment requests.
    decommitment_requests: HistoryRecorder<Vec<()>, H>,
}

impl<S: Storage, const B: bool, H: HistoryMode> DecommitterOracle<B, S, H> {
    pub fn new(storage: StoragePtr<S>) -> Self {
        Self {
            storage,
            known_bytecodes: Default::default(),
            decommitted_code_hashes: Default::default(),
            decommitment_requests: Default::default(),
        }
    }

    /// Gets the bytecode for a given hash (either from storage, or from 'known_bytecodes' that were populated by `populate` method).
    /// Panics if bytecode doesn't exist.
    pub fn get_bytecode(&mut self, hash: U256, timestamp: Timestamp) -> Vec<U256> {
        let entry = self.known_bytecodes.inner().get(&hash);

        match entry {
            Some(x) => x.clone(),
            None => {
                // It is ok to panic here, since the decommitter is never called directly by
                // the users and always called by the VM. VM will never let decommit the
                // code hash which we didn't previously claim to know the preimage of.
                let value = self
                    .storage
                    .borrow_mut()
                    .load_factory_dep(u256_to_h256(hash))
                    .expect("Trying to decode unexisting hash");

                let value = bytes_to_be_words(&value);
                self.known_bytecodes.insert(hash, value.clone(), timestamp);
                value
            }
        }
    }

    /// Adds additional bytecodes. They will take precedent over the bytecodes from storage.
    pub fn populate(&mut self, bytecodes: Vec<(U256, Vec<U256>)>, timestamp: Timestamp) {
        for (hash, bytecode) in bytecodes {
            self.known_bytecodes.insert(hash, bytecode, timestamp);
        }
    }

    pub fn get_used_bytecode_hashes(&self) -> Vec<U256> {
        self.decommitted_code_hashes
            .inner()
            .iter()
            .map(|item| *item.0)
            .collect()
    }

    pub fn get_decommitted_bytecodes_after_timestamp(&self, timestamp: Timestamp) -> usize {
        // Note, that here we rely on the fact that for each used bytecode
        // there is one and only one corresponding event in the history of it.
        self.decommitted_code_hashes
            .history()
            .iter()
            .rev()
            .take_while(|(t, _)| *t >= timestamp)
            .count()
    }

    pub fn get_decommitted_code_hashes_with_history(
        &self,
    ) -> &HistoryRecorder<HashMap<U256, u32>, HistoryEnabled> {
        &self.decommitted_code_hashes
    }

    /// Returns the storage handle. Used only in tests.
    pub fn get_storage(&self) -> StoragePtr<S> {
        self.storage.clone()
    }

    /// Measures the amount of memory used by this Oracle (used for metrics only).
    pub fn get_size(&self) -> usize {
        // Hashmap memory overhead is neglected.
        let known_bytecodes_size = self
            .known_bytecodes
            .inner()
            .iter()
            .map(|(_, value)| value.len() * std::mem::size_of::<U256>())
            .sum::<usize>();
        let decommitted_code_hashes_size =
            self.decommitted_code_hashes.inner().len() * std::mem::size_of::<(U256, u32)>();

        known_bytecodes_size + decommitted_code_hashes_size
    }

    pub fn get_history_size(&self) -> usize {
        let known_bytecodes_stack_size = self.known_bytecodes.borrow_history(|h| h.len(), 0)
            * std::mem::size_of::<<HashMap<U256, Vec<U256>> as WithHistory>::HistoryRecord>();
        let known_bytecodes_heap_size = self.known_bytecodes.borrow_history(
            |h| {
                h.iter()
                    .map(|(_, event)| {
                        if let Some(bytecode) = event.value.as_ref() {
                            bytecode.len() * std::mem::size_of::<U256>()
                        } else {
                            0
                        }
                    })
                    .sum::<usize>()
            },
            0,
        );
        let decommitted_code_hashes_size =
            self.decommitted_code_hashes.borrow_history(|h| h.len(), 0)
                * std::mem::size_of::<<HashMap<U256, u32> as WithHistory>::HistoryRecord>();

        known_bytecodes_stack_size + known_bytecodes_heap_size + decommitted_code_hashes_size
    }

    pub fn delete_history(&mut self) {
        self.decommitted_code_hashes.delete_history();
        self.known_bytecodes.delete_history();
        self.decommitment_requests.delete_history();
    }
}

impl<S: Storage, const B: bool> OracleWithHistory for DecommitterOracle<B, S, HistoryEnabled> {
    fn rollback_to_timestamp(&mut self, timestamp: Timestamp) {
        self.decommitted_code_hashes
            .rollback_to_timestamp(timestamp);
        self.known_bytecodes.rollback_to_timestamp(timestamp);
        self.decommitment_requests.rollback_to_timestamp(timestamp);
    }
}

impl<S: Storage, const B: bool, H: HistoryMode> DecommittmentProcessor
    for DecommitterOracle<B, S, H>
{
    /// Loads a given bytecode hash into memory (see trait description for more details).
    fn decommit_into_memory<M: Memory>(
        &mut self,
        monotonic_cycle_counter: u32,
        mut partial_query: DecommittmentQuery,
        memory: &mut M,
    ) -> (DecommittmentQuery, Option<Vec<U256>>) {
        self.decommitment_requests.push((), partial_query.timestamp);
        // First - check if we didn't fetch this bytecode in the past.
        // If we did - we can just return the page that we used before (as the memory is readonly).
        if let Some(memory_page) = self
            .decommitted_code_hashes
            .inner()
            .get(&partial_query.hash)
            .copied()
        {
            partial_query.is_fresh = false;
            partial_query.memory_page = MemoryPage(memory_page);
            partial_query.decommitted_length =
                bytecode_len_in_words(&u256_to_h256(partial_query.hash));

            (partial_query, None)
        } else {
            // We are fetching a fresh bytecode that we didn't read before.
            let values = self.get_bytecode(partial_query.hash, partial_query.timestamp);
            let page_to_use = partial_query.memory_page;
            let timestamp = partial_query.timestamp;
            partial_query.decommitted_length = values.len() as u16;
            partial_query.is_fresh = true;

            // Create a template query, that we'll use for writing into memory.
            // value & index are set to 0 - as they will be updated in the inner loop below.
            let mut tmp_q = MemoryQuery {
                timestamp,
                location: MemoryLocation {
                    memory_type: MemoryType::Code,
                    page: page_to_use,
                    index: MemoryIndex(0),
                },
                value: U256::zero(),
                value_is_pointer: false,
                rw_flag: true,
                is_pended: false,
            };
            self.decommitted_code_hashes
                .insert(partial_query.hash, page_to_use.0, timestamp);

            // Copy the bytecode (that is stored in 'values' Vec) into the memory page.
            if B {
                for (i, value) in values.iter().enumerate() {
                    tmp_q.location.index = MemoryIndex(i as u32);
                    tmp_q.value = *value;
                    memory.specialized_code_query(monotonic_cycle_counter, tmp_q);
                }
                // If we're in the witness mode - we also have to return the values.
                (partial_query, Some(values))
            } else {
                for (i, value) in values.into_iter().enumerate() {
                    tmp_q.location.index = MemoryIndex(i as u32);
                    tmp_q.value = value;
                    memory.specialized_code_query(monotonic_cycle_counter, tmp_q);
                }

                (partial_query, None)
            }
        }
    }
}

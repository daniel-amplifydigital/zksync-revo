use once_cell::sync::Lazy;
use zk_evm_1_3_1::{
    aux_structures::{LogQuery, MemoryPage, Timestamp},
    block_properties::BlockProperties,
    vm_state::PrimitiveValue,
    zkevm_opcode_defs::FatPointer,
};
use zksync_contracts::BaseSystemContracts;
use zksync_system_constants::ZKPORTER_IS_AVAILABLE;
use zksync_types::{h256_to_u256, Address, StorageLogKind, H160, MAX_L2_TX_GAS_LIMIT, U256};

use crate::{
    glue::GlueInto,
    vm_m5::{memory::SimpleMemory, vm_with_bootloader::BlockContext},
};

pub const INITIAL_TIMESTAMP: u32 = 1024;
pub const INITIAL_MEMORY_COUNTER: u32 = 2048;
pub const INITIAL_CALLDATA_PAGE: u32 = 7;
pub const INITIAL_BASE_PAGE: u32 = 8;
pub const ENTRY_POINT_PAGE: u32 = code_page_candidate_from_base(MemoryPage(INITIAL_BASE_PAGE)).0;

/// How many gas bootloader is allowed to spend within one block.
/// Note that this value doesn't correspond to the gas limit of any particular transaction
/// (except for the fact that, of course, gas limit for each transaction should be <= `BLOCK_GAS_LIMIT`).
pub(crate) const BLOCK_GAS_LIMIT: u32 =
    zk_evm_1_3_1::zkevm_opcode_defs::system_params::VM_INITIAL_FRAME_ERGS;
pub const ETH_CALL_GAS_LIMIT: u32 = MAX_L2_TX_GAS_LIMIT as u32;

#[derive(Debug, Clone)]
pub enum VmExecutionResult {
    Ok(Vec<u8>),
    Revert(Vec<u8>),
    Panic,
    MostLikelyDidNotFinish(Address, u16),
}

pub const fn code_page_candidate_from_base(base: MemoryPage) -> MemoryPage {
    MemoryPage(base.0)
}

pub const fn stack_page_from_base(base: MemoryPage) -> MemoryPage {
    MemoryPage(base.0 + 1)
}

pub const fn heap_page_from_base(base: MemoryPage) -> MemoryPage {
    MemoryPage(base.0 + 2)
}

pub const fn aux_heap_page_from_base(base: MemoryPage) -> MemoryPage {
    MemoryPage(base.0 + 3)
}

pub(crate) fn dump_memory_page_using_primitive_value(
    memory: &SimpleMemory,
    ptr: PrimitiveValue,
) -> Vec<u8> {
    if !ptr.is_pointer {
        return vec![];
    }
    let fat_ptr = FatPointer::from_u256(ptr.value);
    dump_memory_page_using_fat_pointer(memory, fat_ptr)
}

pub(crate) fn dump_memory_page_using_fat_pointer(
    memory: &SimpleMemory,
    fat_ptr: FatPointer,
) -> Vec<u8> {
    dump_memory_page_by_offset_and_length(
        memory,
        fat_ptr.memory_page,
        (fat_ptr.start + fat_ptr.offset) as usize,
        (fat_ptr.length - fat_ptr.offset) as usize,
    )
}

pub(crate) fn dump_memory_page_by_offset_and_length(
    memory: &SimpleMemory,
    page: u32,
    offset: usize,
    length: usize,
) -> Vec<u8> {
    assert!(offset < (1u32 << 24) as usize);
    assert!(length < (1u32 << 24) as usize);
    let mut dump = Vec::with_capacity(length);
    if length == 0 {
        return dump;
    }

    let first_word = offset / 32;
    let end_byte = offset + length;
    let mut last_word = end_byte / 32;
    if end_byte % 32 != 0 {
        last_word += 1;
    }

    let unalignment = offset % 32;

    let page_part =
        memory.dump_page_content_as_u256_words(page, (first_word as u32)..(last_word as u32));

    let mut is_first = true;
    let mut remaining = length;
    for word in page_part.into_iter() {
        let it = word.into_be_iter();
        if is_first {
            is_first = false;
            let it = it.skip(unalignment);
            for next in it {
                if remaining > 0 {
                    dump.push(next);
                    remaining -= 1;
                }
            }
        } else {
            for next in it {
                if remaining > 0 {
                    dump.push(next);
                    remaining -= 1;
                }
            }
        }
    }

    assert_eq!(
        dump.len(),
        length,
        "tried to dump with offset {}, length {}, got a bytestring of length {}",
        offset,
        length,
        dump.len()
    );

    dump
}

pub trait FixedLengthIterator<'a, I: 'a, const N: usize>: Iterator<Item = I>
where
    Self: 'a,
{
    fn next(&mut self) -> Option<<Self as Iterator>::Item> {
        <Self as Iterator>::next(self)
    }
}

pub trait IntoFixedLengthByteIterator<const N: usize> {
    type IntoIter: FixedLengthIterator<'static, u8, N>;
    fn into_le_iter(self) -> Self::IntoIter;
    fn into_be_iter(self) -> Self::IntoIter;
}

pub struct FixedBufferValueIterator<T, const N: usize> {
    iter: std::array::IntoIter<T, N>,
}

impl<T: Clone, const N: usize> Iterator for FixedBufferValueIterator<T, N> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next()
    }
}

impl<T: Clone + 'static, const N: usize> FixedLengthIterator<'static, T, N>
    for FixedBufferValueIterator<T, N>
{
}

impl IntoFixedLengthByteIterator<32> for U256 {
    type IntoIter = FixedBufferValueIterator<u8, 32>;
    fn into_le_iter(self) -> Self::IntoIter {
        let mut buffer = [0u8; 32];
        self.to_little_endian(&mut buffer);

        FixedBufferValueIterator {
            iter: IntoIterator::into_iter(buffer),
        }
    }

    fn into_be_iter(self) -> Self::IntoIter {
        let mut buffer = [0u8; 32];
        self.to_big_endian(&mut buffer);

        FixedBufferValueIterator {
            iter: IntoIterator::into_iter(buffer),
        }
    }
}

/// Collects storage log queries where `log.log_query.timestamp >= from_timestamp`.
/// Denote `n` to be the number of such queries, then it works in O(n).
pub fn collect_storage_log_queries_after_timestamp(
    all_log_queries: &[StorageLogQuery],
    from_timestamp: Timestamp,
) -> Vec<StorageLogQuery> {
    let from_timestamp = from_timestamp.glue_into();
    all_log_queries
        .iter()
        .rev()
        .take_while(|log_query| log_query.log_query.timestamp >= from_timestamp)
        .cloned()
        .collect::<Vec<StorageLogQuery>>()
        .into_iter()
        .rev()
        .collect()
}

/// Collects all log queries where `log_query.timestamp >= from_timestamp`.
/// Denote `n` to be the number of such queries, then it works in O(n).
pub fn collect_log_queries_after_timestamp(
    all_log_queries: &[LogQuery],
    from_timestamp: Timestamp,
) -> Vec<LogQuery> {
    all_log_queries
        .iter()
        .rev()
        .take_while(|log_query| log_query.timestamp >= from_timestamp)
        .cloned()
        .collect::<Vec<LogQuery>>()
        .into_iter()
        .rev()
        .collect()
}

/// Receives sorted slice of timestamps.
/// Returns count of timestamps that are greater than or equal to `from_timestamp`.
/// Works in `O(log(sorted_timestamps.len()))`.
pub fn precompile_calls_count_after_timestamp(
    sorted_timestamps: &[Timestamp],
    from_timestamp: Timestamp,
) -> usize {
    sorted_timestamps.len() - sorted_timestamps.partition_point(|t| *t < from_timestamp)
}

pub static BASE_SYSTEM_CONTRACTS: Lazy<BaseSystemContracts> =
    Lazy::new(BaseSystemContracts::load_from_disk);

pub fn create_test_block_params() -> (BlockContext, BlockProperties) {
    let context = BlockContext {
        block_number: 1u32,
        block_timestamp: 1000,
        l1_gas_price: 50_000_000_000,   // 50 gwei
        fair_l2_gas_price: 250_000_000, // 0.25 gwei
        operator_address: H160::zero(),
    };

    (
        context,
        BlockProperties {
            default_aa_code_hash: h256_to_u256(BASE_SYSTEM_CONTRACTS.default_aa.hash),
            zkporter_is_available: ZKPORTER_IS_AVAILABLE,
        },
    )
}

/// Log query, which handle initial and repeated writes to the storage
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageLogQuery {
    pub log_query: LogQuery,
    pub log_type: StorageLogKind,
}

//! Prover and server subsystems communicate via the API.
//! This module defines the types used in the API.

use serde::{Deserialize, Serialize};
use serde_with::{hex::Hex, serde_as};
use zksync_types::{
    protocol_version::{L1VerifierConfig, ProtocolSemanticVersion},
    tee_types::TeeType,
    L1BatchNumber,
};

use crate::{
    inputs::{TeeVerifierInput, WitnessInputData},
    outputs::{JsonL1BatchProofForL1, L1BatchTeeProofForL1},
};

// Structs for holding data returned in HTTP responses

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofGenerationData {
    pub l1_batch_number: L1BatchNumber,
    #[serde(default = "chrono::Utc::now")]
    pub batch_sealed_at: chrono::DateTime<chrono::Utc>,
    pub witness_input_data: WitnessInputData,
    pub protocol_version: ProtocolSemanticVersion,
    pub l1_verifier_config: L1VerifierConfig,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ProofGenerationDataResponse {
    Success(Option<Box<ProofGenerationData>>),
    Error(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TeeProofGenerationDataResponse(pub Box<TeeVerifierInput>);

#[derive(Debug, Serialize, Deserialize)]
pub enum SubmitProofResponse {
    Success,
    Error(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum SubmitTeeProofResponse {
    Success,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RegisterTeeAttestationResponse {
    Success,
}

// Structs to hold data necessary for making HTTP requests

#[derive(Debug, Serialize, Deserialize)]
pub struct ProofGenerationDataRequest {}

#[derive(Debug, Serialize, Deserialize)]
pub struct TeeProofGenerationDataRequest {
    pub tee_type: TeeType,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum SubmitProofRequest {
    Proof(Box<JsonL1BatchProofForL1>),
    // The proof generation was skipped due to sampling
    SkippedProofGeneration,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerifyProofRequest(pub Box<JsonL1BatchProofForL1>);

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SubmitTeeProofRequest(pub Box<L1BatchTeeProofForL1>);

#[serde_as]
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RegisterTeeAttestationRequest {
    #[serde_as(as = "Hex")]
    pub attestation: Vec<u8>,
    #[serde_as(as = "Hex")]
    pub pubkey: Vec<u8>,
}

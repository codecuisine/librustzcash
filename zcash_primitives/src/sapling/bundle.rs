use core::fmt::Debug;

use memuse::DynamicUsage;

use zcash_note_encryption::{
    EphemeralKeyBytes, ShieldedOutput, COMPACT_NOTE_SIZE, ENC_CIPHERTEXT_SIZE, OUT_CIPHERTEXT_SIZE,
};

use crate::{
    consensus,
    sapling::{
        note::ExtractedNoteCommitment,
        note_encryption::{CompactOutputDescription, SaplingDomain},
        redjubjub::{self, PublicKey, Signature},
        value::ValueCommitment,
        Nullifier,
    },
    transaction::components::{Amount, GROTH_PROOF_SIZE},
};

pub type GrothProofBytes = [u8; GROTH_PROOF_SIZE];

/// Defines the authorization type of a Sapling bundle.
pub trait Authorization: Debug {
    type SpendProof: Clone + Debug;
    type OutputProof: Clone + Debug;
    type AuthSig: Clone + Debug;
}

/// Authorizing data for a bundle of Sapling spends and outputs, ready to be committed to
/// the ledger.
#[derive(Debug, Copy, Clone)]
pub struct Authorized {
    // TODO: Make this private.
    pub binding_sig: redjubjub::Signature,
}

impl Authorization for Authorized {
    type SpendProof = GrothProofBytes;
    type OutputProof = GrothProofBytes;
    type AuthSig = redjubjub::Signature;
}

/// A map from one bundle authorization to another.
///
/// For use with [`Bundle::map_authorization`].
pub trait MapAuth<A: Authorization, B: Authorization> {
    fn map_spend_proof(&mut self, p: A::SpendProof) -> B::SpendProof;
    fn map_output_proof(&mut self, p: A::OutputProof) -> B::OutputProof;
    fn map_auth_sig(&mut self, s: A::AuthSig) -> B::AuthSig;
    fn map_authorization(&mut self, a: A) -> B;
}

/// The identity map.
///
/// This can be used with [`TransactionData::map_authorization`] when you want to map the
/// authorization of a subset of the transaction's bundles.
///
/// [`TransactionData::map_authorization`]: crate::transaction::TransactionData::map_authorization
impl MapAuth<Authorized, Authorized> for () {
    fn map_spend_proof(
        &mut self,
        p: <Authorized as Authorization>::SpendProof,
    ) -> <Authorized as Authorization>::SpendProof {
        p
    }

    fn map_output_proof(
        &mut self,
        p: <Authorized as Authorization>::OutputProof,
    ) -> <Authorized as Authorization>::OutputProof {
        p
    }

    fn map_auth_sig(
        &mut self,
        s: <Authorized as Authorization>::AuthSig,
    ) -> <Authorized as Authorization>::AuthSig {
        s
    }

    fn map_authorization(&mut self, a: Authorized) -> Authorized {
        a
    }
}

/// A helper for implementing `MapAuth` with a set of closures.
impl<A, B, F, G, H, I> MapAuth<A, B> for (F, G, H, I)
where
    A: Authorization,
    B: Authorization,
    F: FnMut(A::SpendProof) -> B::SpendProof,
    G: FnMut(A::OutputProof) -> B::OutputProof,
    H: FnMut(A::AuthSig) -> B::AuthSig,
    I: FnMut(A) -> B,
{
    fn map_spend_proof(&mut self, p: A::SpendProof) -> B::SpendProof {
        self.0(p)
    }

    fn map_output_proof(&mut self, p: A::OutputProof) -> B::OutputProof {
        self.1(p)
    }

    fn map_auth_sig(&mut self, s: A::AuthSig) -> B::AuthSig {
        self.2(s)
    }

    fn map_authorization(&mut self, a: A) -> B {
        self.3(a)
    }
}

/// A fallible map from one bundle authorization to another.
///
/// For use with [`Bundle::try_map_authorization`].
pub trait TryMapAuth<A: Authorization, B: Authorization> {
    type Error;
    fn try_map_spend_proof(&mut self, p: A::SpendProof) -> Result<B::SpendProof, Self::Error>;
    fn try_map_output_proof(&mut self, p: A::OutputProof) -> Result<B::OutputProof, Self::Error>;
    fn try_map_auth_sig(&mut self, s: A::AuthSig) -> Result<B::AuthSig, Self::Error>;
    fn try_map_authorization(&mut self, a: A) -> Result<B, Self::Error>;
}

/// A helper for implementing `TryMapAuth` with a set of closures.
impl<A, B, E, F, G, H, I> TryMapAuth<A, B> for (F, G, H, I)
where
    A: Authorization,
    B: Authorization,
    F: FnMut(A::SpendProof) -> Result<B::SpendProof, E>,
    G: FnMut(A::OutputProof) -> Result<B::OutputProof, E>,
    H: FnMut(A::AuthSig) -> Result<B::AuthSig, E>,
    I: FnMut(A) -> Result<B, E>,
{
    type Error = E;

    fn try_map_spend_proof(&mut self, p: A::SpendProof) -> Result<B::SpendProof, Self::Error> {
        self.0(p)
    }

    fn try_map_output_proof(&mut self, p: A::OutputProof) -> Result<B::OutputProof, Self::Error> {
        self.1(p)
    }

    fn try_map_auth_sig(&mut self, s: A::AuthSig) -> Result<B::AuthSig, Self::Error> {
        self.2(s)
    }

    fn try_map_authorization(&mut self, a: A) -> Result<B, Self::Error> {
        self.3(a)
    }
}

#[derive(Debug, Clone)]
pub struct Bundle<A: Authorization> {
    shielded_spends: Vec<SpendDescription<A>>,
    shielded_outputs: Vec<OutputDescription<A::OutputProof>>,
    value_balance: Amount,
    authorization: A,
}

impl<A: Authorization> Bundle<A> {
    /// Constructs a `Bundle` from its constituent parts.
    #[cfg(feature = "temporary-zcashd")]
    pub fn temporary_zcashd_from_parts(
        shielded_spends: Vec<SpendDescription<A>>,
        shielded_outputs: Vec<OutputDescription<A::OutputProof>>,
        value_balance: Amount,
        authorization: A,
    ) -> Self {
        Self::from_parts(
            shielded_spends,
            shielded_outputs,
            value_balance,
            authorization,
        )
    }

    /// Constructs a `Bundle` from its constituent parts.
    pub(crate) fn from_parts(
        shielded_spends: Vec<SpendDescription<A>>,
        shielded_outputs: Vec<OutputDescription<A::OutputProof>>,
        value_balance: Amount,
        authorization: A,
    ) -> Self {
        Bundle {
            shielded_spends,
            shielded_outputs,
            value_balance,
            authorization,
        }
    }

    /// Returns the list of spends in this bundle.
    pub fn shielded_spends(&self) -> &[SpendDescription<A>] {
        &self.shielded_spends
    }

    /// Returns the list of outputs in this bundle.
    pub fn shielded_outputs(&self) -> &[OutputDescription<A::OutputProof>] {
        &self.shielded_outputs
    }

    /// Returns the net value moved into or out of the Sapling shielded pool.
    ///
    /// This is the sum of Sapling spends minus the sum of Sapling outputs.
    pub fn value_balance(&self) -> &Amount {
        &self.value_balance
    }

    /// Returns the authorization for this bundle.
    ///
    /// In the case of a `Bundle<Authorized>`, this is the binding signature.
    pub fn authorization(&self) -> &A {
        &self.authorization
    }

    /// Transitions this bundle from one authorization state to another.
    pub fn map_authorization<B: Authorization, F: MapAuth<A, B>>(self, mut f: F) -> Bundle<B> {
        Bundle {
            shielded_spends: self
                .shielded_spends
                .into_iter()
                .map(|d| SpendDescription {
                    cv: d.cv,
                    anchor: d.anchor,
                    nullifier: d.nullifier,
                    rk: d.rk,
                    zkproof: f.map_spend_proof(d.zkproof),
                    spend_auth_sig: f.map_auth_sig(d.spend_auth_sig),
                })
                .collect(),
            shielded_outputs: self
                .shielded_outputs
                .into_iter()
                .map(|o| OutputDescription {
                    cv: o.cv,
                    cmu: o.cmu,
                    ephemeral_key: o.ephemeral_key,
                    enc_ciphertext: o.enc_ciphertext,
                    out_ciphertext: o.out_ciphertext,
                    zkproof: f.map_output_proof(o.zkproof),
                })
                .collect(),
            value_balance: self.value_balance,
            authorization: f.map_authorization(self.authorization),
        }
    }

    /// Transitions this bundle from one authorization state to another.
    pub fn try_map_authorization<B: Authorization, F: TryMapAuth<A, B>>(
        self,
        mut f: F,
    ) -> Result<Bundle<B>, F::Error> {
        Ok(Bundle {
            shielded_spends: self
                .shielded_spends
                .into_iter()
                .map(|d| {
                    Ok(SpendDescription {
                        cv: d.cv,
                        anchor: d.anchor,
                        nullifier: d.nullifier,
                        rk: d.rk,
                        zkproof: f.try_map_spend_proof(d.zkproof)?,
                        spend_auth_sig: f.try_map_auth_sig(d.spend_auth_sig)?,
                    })
                })
                .collect::<Result<_, _>>()?,
            shielded_outputs: self
                .shielded_outputs
                .into_iter()
                .map(|o| {
                    Ok(OutputDescription {
                        cv: o.cv,
                        cmu: o.cmu,
                        ephemeral_key: o.ephemeral_key,
                        enc_ciphertext: o.enc_ciphertext,
                        out_ciphertext: o.out_ciphertext,
                        zkproof: f.try_map_output_proof(o.zkproof)?,
                    })
                })
                .collect::<Result<_, _>>()?,
            value_balance: self.value_balance,
            authorization: f.try_map_authorization(self.authorization)?,
        })
    }
}

impl DynamicUsage for Bundle<Authorized> {
    fn dynamic_usage(&self) -> usize {
        self.shielded_spends.dynamic_usage() + self.shielded_outputs.dynamic_usage()
    }

    fn dynamic_usage_bounds(&self) -> (usize, Option<usize>) {
        let bounds = (
            self.shielded_spends.dynamic_usage_bounds(),
            self.shielded_outputs.dynamic_usage_bounds(),
        );

        (
            bounds.0 .0 + bounds.1 .0,
            bounds.0 .1.zip(bounds.1 .1).map(|(a, b)| a + b),
        )
    }
}

#[derive(Clone)]
pub struct SpendDescription<A: Authorization> {
    cv: ValueCommitment,
    anchor: bls12_381::Scalar,
    nullifier: Nullifier,
    rk: PublicKey,
    zkproof: A::SpendProof,
    spend_auth_sig: A::AuthSig,
}

impl<A: Authorization> std::fmt::Debug for SpendDescription<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "SpendDescription(cv = {:?}, anchor = {:?}, nullifier = {:?}, rk = {:?}, spend_auth_sig = {:?})",
            self.cv, self.anchor, self.nullifier, self.rk, self.spend_auth_sig
        )
    }
}

impl<A: Authorization> SpendDescription<A> {
    #[cfg(feature = "temporary-zcashd")]
    pub fn temporary_zcashd_from_parts(
        cv: ValueCommitment,
        anchor: bls12_381::Scalar,
        nullifier: Nullifier,
        rk: PublicKey,
        zkproof: A::SpendProof,
        spend_auth_sig: A::AuthSig,
    ) -> Self {
        Self::from_parts(cv, anchor, nullifier, rk, zkproof, spend_auth_sig)
    }

    pub(crate) fn from_parts(
        cv: ValueCommitment,
        anchor: bls12_381::Scalar,
        nullifier: Nullifier,
        rk: PublicKey,
        zkproof: A::SpendProof,
        spend_auth_sig: A::AuthSig,
    ) -> Self {
        Self {
            cv,
            anchor,
            nullifier,
            rk,
            zkproof,
            spend_auth_sig,
        }
    }

    /// Returns the commitment to the value consumed by this spend.
    pub fn cv(&self) -> &ValueCommitment {
        &self.cv
    }

    /// Returns the root of the Sapling commitment tree that this spend commits to.
    pub fn anchor(&self) -> &bls12_381::Scalar {
        &self.anchor
    }

    /// Returns the nullifier of the note being spent.
    pub fn nullifier(&self) -> &Nullifier {
        &self.nullifier
    }

    /// Returns the randomized verification key for the note being spent.
    pub fn rk(&self) -> &PublicKey {
        &self.rk
    }

    /// Returns the proof for this spend.
    pub fn zkproof(&self) -> &A::SpendProof {
        &self.zkproof
    }

    /// Returns the authorization signature for this spend.
    pub fn spend_auth_sig(&self) -> &A::AuthSig {
        &self.spend_auth_sig
    }
}

impl DynamicUsage for SpendDescription<Authorized> {
    fn dynamic_usage(&self) -> usize {
        self.zkproof.dynamic_usage()
    }

    fn dynamic_usage_bounds(&self) -> (usize, Option<usize>) {
        self.zkproof.dynamic_usage_bounds()
    }
}

#[derive(Clone)]
pub struct SpendDescriptionV5 {
    cv: ValueCommitment,
    nullifier: Nullifier,
    rk: PublicKey,
}

impl SpendDescriptionV5 {
    pub(crate) fn from_parts(cv: ValueCommitment, nullifier: Nullifier, rk: PublicKey) -> Self {
        Self { cv, nullifier, rk }
    }

    pub fn into_spend_description(
        self,
        anchor: bls12_381::Scalar,
        zkproof: GrothProofBytes,
        spend_auth_sig: Signature,
    ) -> SpendDescription<Authorized> {
        SpendDescription {
            cv: self.cv,
            anchor,
            nullifier: self.nullifier,
            rk: self.rk,
            zkproof,
            spend_auth_sig,
        }
    }
}

#[derive(Clone)]
pub struct OutputDescription<Proof> {
    cv: ValueCommitment,
    cmu: ExtractedNoteCommitment,
    ephemeral_key: EphemeralKeyBytes,
    enc_ciphertext: [u8; ENC_CIPHERTEXT_SIZE],
    out_ciphertext: [u8; OUT_CIPHERTEXT_SIZE],
    zkproof: Proof,
}

impl<Proof> OutputDescription<Proof> {
    /// Returns the commitment to the value consumed by this output.
    pub fn cv(&self) -> &ValueCommitment {
        &self.cv
    }

    /// Returns the commitment to the new note being created.
    pub fn cmu(&self) -> &ExtractedNoteCommitment {
        &self.cmu
    }

    pub fn ephemeral_key(&self) -> &EphemeralKeyBytes {
        &self.ephemeral_key
    }

    /// Returns the encrypted note ciphertext.
    pub fn enc_ciphertext(&self) -> &[u8; ENC_CIPHERTEXT_SIZE] {
        &self.enc_ciphertext
    }

    /// Returns the output recovery ciphertext.
    pub fn out_ciphertext(&self) -> &[u8; OUT_CIPHERTEXT_SIZE] {
        &self.out_ciphertext
    }

    /// Returns the proof for this output.
    pub fn zkproof(&self) -> &Proof {
        &self.zkproof
    }

    #[cfg(feature = "temporary-zcashd")]
    pub fn temporary_zcashd_from_parts(
        cv: ValueCommitment,
        cmu: ExtractedNoteCommitment,
        ephemeral_key: EphemeralKeyBytes,
        enc_ciphertext: [u8; ENC_CIPHERTEXT_SIZE],
        out_ciphertext: [u8; OUT_CIPHERTEXT_SIZE],
        zkproof: Proof,
    ) -> Self {
        Self::from_parts(
            cv,
            cmu,
            ephemeral_key,
            enc_ciphertext,
            out_ciphertext,
            zkproof,
        )
    }

    pub(crate) fn from_parts(
        cv: ValueCommitment,
        cmu: ExtractedNoteCommitment,
        ephemeral_key: EphemeralKeyBytes,
        enc_ciphertext: [u8; ENC_CIPHERTEXT_SIZE],
        out_ciphertext: [u8; OUT_CIPHERTEXT_SIZE],
        zkproof: Proof,
    ) -> Self {
        OutputDescription {
            cv,
            cmu,
            ephemeral_key,
            enc_ciphertext,
            out_ciphertext,
            zkproof,
        }
    }
}

#[cfg(test)]
impl<Proof> OutputDescription<Proof> {
    pub(crate) fn cv_mut(&mut self) -> &mut ValueCommitment {
        &mut self.cv
    }
    pub(crate) fn cmu_mut(&mut self) -> &mut ExtractedNoteCommitment {
        &mut self.cmu
    }
    pub(crate) fn ephemeral_key_mut(&mut self) -> &mut EphemeralKeyBytes {
        &mut self.ephemeral_key
    }
    pub(crate) fn enc_ciphertext_mut(&mut self) -> &mut [u8; ENC_CIPHERTEXT_SIZE] {
        &mut self.enc_ciphertext
    }
    pub(crate) fn out_ciphertext_mut(&mut self) -> &mut [u8; OUT_CIPHERTEXT_SIZE] {
        &mut self.out_ciphertext
    }
}

impl<Proof: DynamicUsage> DynamicUsage for OutputDescription<Proof> {
    fn dynamic_usage(&self) -> usize {
        self.zkproof.dynamic_usage()
    }

    fn dynamic_usage_bounds(&self) -> (usize, Option<usize>) {
        self.zkproof.dynamic_usage_bounds()
    }
}

impl<P: consensus::Parameters, A> ShieldedOutput<SaplingDomain<P>, ENC_CIPHERTEXT_SIZE>
    for OutputDescription<A>
{
    fn ephemeral_key(&self) -> EphemeralKeyBytes {
        self.ephemeral_key.clone()
    }

    fn cmstar_bytes(&self) -> [u8; 32] {
        self.cmu.to_bytes()
    }

    fn enc_ciphertext(&self) -> &[u8; ENC_CIPHERTEXT_SIZE] {
        &self.enc_ciphertext
    }
}

impl<A> std::fmt::Debug for OutputDescription<A> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "OutputDescription(cv = {:?}, cmu = {:?}, ephemeral_key = {:?})",
            self.cv, self.cmu, self.ephemeral_key
        )
    }
}

#[derive(Clone)]
pub struct OutputDescriptionV5 {
    cv: ValueCommitment,
    cmu: ExtractedNoteCommitment,
    ephemeral_key: EphemeralKeyBytes,
    enc_ciphertext: [u8; ENC_CIPHERTEXT_SIZE],
    out_ciphertext: [u8; OUT_CIPHERTEXT_SIZE],
}

memuse::impl_no_dynamic_usage!(OutputDescriptionV5);

impl OutputDescriptionV5 {
    pub(crate) fn from_parts(
        cv: ValueCommitment,
        cmu: ExtractedNoteCommitment,
        ephemeral_key: EphemeralKeyBytes,
        enc_ciphertext: [u8; ENC_CIPHERTEXT_SIZE],
        out_ciphertext: [u8; OUT_CIPHERTEXT_SIZE],
    ) -> Self {
        Self {
            cv,
            cmu,
            ephemeral_key,
            enc_ciphertext,
            out_ciphertext,
        }
    }

    pub fn into_output_description(
        self,
        zkproof: GrothProofBytes,
    ) -> OutputDescription<GrothProofBytes> {
        OutputDescription {
            cv: self.cv,
            cmu: self.cmu,
            ephemeral_key: self.ephemeral_key,
            enc_ciphertext: self.enc_ciphertext,
            out_ciphertext: self.out_ciphertext,
            zkproof,
        }
    }
}

impl<A> From<OutputDescription<A>> for CompactOutputDescription {
    fn from(out: OutputDescription<A>) -> CompactOutputDescription {
        CompactOutputDescription {
            ephemeral_key: out.ephemeral_key,
            cmu: out.cmu,
            enc_ciphertext: out.enc_ciphertext[..COMPACT_NOTE_SIZE].try_into().unwrap(),
        }
    }
}

#[cfg(any(test, feature = "test-dependencies"))]
pub mod testing {
    use ff::Field;
    use group::{Group, GroupEncoding};
    use proptest::collection::vec;
    use proptest::prelude::*;
    use rand::{rngs::StdRng, SeedableRng};

    use crate::{
        sapling::{
            constants::{SPENDING_KEY_GENERATOR, VALUE_COMMITMENT_RANDOMNESS_GENERATOR},
            note::ExtractedNoteCommitment,
            redjubjub::{PrivateKey, PublicKey},
            value::{
                testing::{arb_note_value_bounded, arb_trapdoor},
                ValueCommitment, MAX_NOTE_VALUE,
            },
            Nullifier,
        },
        transaction::components::{amount::testing::arb_amount, GROTH_PROOF_SIZE},
    };

    use super::{
        Authorized, Bundle, GrothProofBytes, OutputDescription, SpendDescription,
        ENC_CIPHERTEXT_SIZE, OUT_CIPHERTEXT_SIZE,
    };

    prop_compose! {
        fn arb_extended_point()(rng_seed in prop::array::uniform32(any::<u8>())) -> jubjub::ExtendedPoint {
            let mut rng = StdRng::from_seed(rng_seed);
            let scalar = jubjub::Scalar::random(&mut rng);
            jubjub::ExtendedPoint::generator() * scalar
        }
    }

    prop_compose! {
        /// produce a spend description with invalid data (useful only for serialization
        /// roundtrip testing).
        fn arb_spend_description(n_spends: usize)(
            value in arb_note_value_bounded(MAX_NOTE_VALUE.checked_div(n_spends as u64).unwrap_or(0)),
            rcv in arb_trapdoor(),
            anchor in vec(any::<u8>(), 64)
                .prop_map(|v| <[u8;64]>::try_from(v.as_slice()).unwrap())
                .prop_map(|v| bls12_381::Scalar::from_bytes_wide(&v)),
            nullifier in prop::array::uniform32(any::<u8>())
                .prop_map(|v| Nullifier::from_slice(&v).unwrap()),
            zkproof in vec(any::<u8>(), GROTH_PROOF_SIZE)
                .prop_map(|v| <[u8;GROTH_PROOF_SIZE]>::try_from(v.as_slice()).unwrap()),
            rng_seed in prop::array::uniform32(prop::num::u8::ANY),
            fake_sighash_bytes in prop::array::uniform32(prop::num::u8::ANY),
        ) -> SpendDescription<Authorized> {
            let mut rng = StdRng::from_seed(rng_seed);
            let sk1 = PrivateKey(jubjub::Fr::random(&mut rng));
            let rk = PublicKey::from_private(&sk1, SPENDING_KEY_GENERATOR);
            let cv = ValueCommitment::derive(value, rcv);
            SpendDescription {
                cv,
                anchor,
                nullifier,
                rk,
                zkproof,
                spend_auth_sig: sk1.sign(&fake_sighash_bytes, &mut rng, SPENDING_KEY_GENERATOR),
            }
        }
    }

    prop_compose! {
        /// produce an output description with invalid data (useful only for serialization
        /// roundtrip testing).
        pub fn arb_output_description(n_outputs: usize)(
            value in arb_note_value_bounded(MAX_NOTE_VALUE.checked_div(n_outputs as u64).unwrap_or(0)),
            rcv in arb_trapdoor(),
            cmu in vec(any::<u8>(), 64)
                .prop_map(|v| <[u8;64]>::try_from(v.as_slice()).unwrap())
                .prop_map(|v| bls12_381::Scalar::from_bytes_wide(&v)),
            enc_ciphertext in vec(any::<u8>(), ENC_CIPHERTEXT_SIZE)
                .prop_map(|v| <[u8; ENC_CIPHERTEXT_SIZE]>::try_from(v.as_slice()).unwrap()),
            epk in arb_extended_point(),
            out_ciphertext in vec(any::<u8>(), OUT_CIPHERTEXT_SIZE)
                .prop_map(|v| <[u8; OUT_CIPHERTEXT_SIZE]>::try_from(v.as_slice()).unwrap()),
            zkproof in vec(any::<u8>(), GROTH_PROOF_SIZE)
                .prop_map(|v| <[u8; GROTH_PROOF_SIZE]>::try_from(v.as_slice()).unwrap()),
        ) -> OutputDescription<GrothProofBytes> {
            let cv = ValueCommitment::derive(value, rcv);
            let cmu = ExtractedNoteCommitment::from_bytes(&cmu.to_bytes()).unwrap();
            OutputDescription {
                cv,
                cmu,
                ephemeral_key: epk.to_bytes().into(),
                enc_ciphertext,
                out_ciphertext,
                zkproof,
            }
        }
    }

    prop_compose! {
        pub fn arb_bundle()(
            n_spends in 0usize..30,
            n_outputs in 0usize..30,
        )(
            shielded_spends in vec(arb_spend_description(n_spends), n_spends),
            shielded_outputs in vec(arb_output_description(n_outputs), n_outputs),
            value_balance in arb_amount(),
            rng_seed in prop::array::uniform32(prop::num::u8::ANY),
            fake_bvk_bytes in prop::array::uniform32(prop::num::u8::ANY),
        ) -> Option<Bundle<Authorized>> {
            if shielded_spends.is_empty() && shielded_outputs.is_empty() {
                None
            } else {
                let mut rng = StdRng::from_seed(rng_seed);
                let bsk = PrivateKey(jubjub::Fr::random(&mut rng));

                Some(
                    Bundle {
                        shielded_spends,
                        shielded_outputs,
                        value_balance,
                        authorization: Authorized { binding_sig: bsk.sign(&fake_bvk_bytes, &mut rng, VALUE_COMMITMENT_RANDOMNESS_GENERATOR) },
                    }
                )
            }
        }
    }
}

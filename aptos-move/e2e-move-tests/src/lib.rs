// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use aptos::common::types::MovePackageDir;
use aptos::move_tool::{BuiltPackage, MemberId};
use aptos_types::access_path;
use aptos_types::account_address::AccountAddress;
use aptos_types::transaction::{
    ScriptFunction, SignedTransaction, TransactionPayload, TransactionStatus,
};
use cached_framework_packages::aptos_stdlib;
use framework::natives::code::UpgradePolicy;
use language_e2e_tests::account::{Account, AccountData};
use language_e2e_tests::executor::FakeExecutor;
use move_deps::move_core_types::language_storage::{StructTag, TypeTag};
use project_root::get_project_root;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::path::Path;

/// A simple test harness for defining Move e2e tests.
///
/// Tests defined via this harness typically live in the `<crate>/tests` directory, the standard
/// Rust place for defining integration tests.
///
/// For defining a set of new tests around a specific area, you add a new Rust source
/// `tested_area.rs` to the `tests` directory of your crate. You also will create a directory
/// `tested_area.data` which lives side-by-side with the Rust source. In this directory, you
/// place any number of Move packages you need for running the tests. In addition, the test
/// infrastructure will place baseline (golden) files in the `tested_area.data` using the `.exp`
/// (expected) ending.  For examples, see e.g. the `tests/code_publishing.rs` test in this crate.
///
/// NOTE: This harness currently is a wrapper around existing legacy e2e testing infra. We
/// eventually plan to retire the legacy code, and are rather keen to know what of the legacy
/// test infra we want to maintain and also which existing tests to preserve.
pub struct MoveHarness {
    /// The executor being used.
    executor: FakeExecutor,
    /// The current transaction sequence number, by account address.
    txn_seq_no: BTreeMap<AccountAddress, u64>,
}

impl MoveHarness {
    /// Creates a new harness.
    pub fn new() -> Self {
        Self {
            executor: FakeExecutor::from_fresh_genesis(),
            txn_seq_no: BTreeMap::default(),
        }
    }

    pub fn new_no_parallel() -> Self {
        Self {
            executor: FakeExecutor::from_fresh_genesis().set_not_parallel(),
            txn_seq_no: BTreeMap::default(),
        }
    }

    /// Creates an account for the given static address. This address needs to be static so
    /// we can load regular Move code to there without need to rewrite code addresses.
    pub fn new_account_at(&mut self, addr: AccountAddress) -> Account {
        // The below will use the genesis keypair but that should be fine.
        let acc = Account::new_genesis_account(addr);
        let data = AccountData::with_account(acc, 1_000_000, 10);
        self.txn_seq_no.insert(addr, 10);
        self.executor.add_account_data(&data);
        data.account().clone()
    }

    /// Runs a signed transaction. On success, applies the write set.
    pub fn run(&mut self, txn: SignedTransaction) -> TransactionStatus {
        let output = self.executor.execute_transaction(txn);
        if matches!(output.status(), TransactionStatus::Keep(_)) {
            self.executor.apply_write_set(output.write_set());
        }
        output.status().to_owned()
    }

    /// Runs a block of signed transactions. On success, applies the write set.
    pub fn run_block(&mut self, txn_block: Vec<SignedTransaction>) -> Vec<TransactionStatus> {
        let mut result = vec![];
        for output in self.executor.execute_block(txn_block).unwrap() {
            if matches!(output.status(), TransactionStatus::Keep(_)) {
                self.executor.apply_write_set(output.write_set());
            }
            result.push(output.status().to_owned())
        }
        result
    }

    /// Creates a transaction, based on provided payload.
    pub fn create_transaction_payload(
        &mut self,
        account: &Account,
        payload: TransactionPayload,
    ) -> SignedTransaction {
        // We initialize for some reason with 10, so use 10 as the first value here too
        let seq_no_ref = self.txn_seq_no.get_mut(account.address()).unwrap();
        let seq_no = *seq_no_ref;
        *seq_no_ref += 1;
        account
            .transaction()
            .sequence_number(seq_no)
            .gas_unit_price(1)
            .payload(payload)
            .sign()
    }

    /// Runs a transaction, based on provided payload. If the transaction succeeds, any generated
    /// writeset will be applied to storage.
    pub fn run_transaction_payload(
        &mut self,
        account: &Account,
        payload: TransactionPayload,
    ) -> TransactionStatus {
        let txn = self.create_transaction_payload(account, payload);
        self.run(txn)
    }

    /// Creates a transaction which runs the specified entry point `fun`. Arguments need to be
    /// provided in bcs-serialized form.
    pub fn create_entry_function(
        &mut self,
        account: &Account,
        fun: MemberId,
        ty_args: Vec<TypeTag>,
        args: Vec<Vec<u8>>,
    ) -> SignedTransaction {
        let MemberId {
            module_id,
            member_id: function_id,
        } = fun;
        self.create_transaction_payload(
            account,
            TransactionPayload::ScriptFunction(ScriptFunction::new(
                module_id,
                function_id,
                ty_args,
                args,
            )),
        )
    }

    /// Run the specified entry point `fun`. Arguments need to be provided in bcs-serialized form.
    pub fn run_entry_function(
        &mut self,
        account: &Account,
        fun: MemberId,
        ty_args: Vec<TypeTag>,
        args: Vec<Vec<u8>>,
    ) -> TransactionStatus {
        let txn = self.create_entry_function(account, fun, ty_args, args);
        self.run(txn)
    }

    /// Creates a transaction which publishes the Move Package found at the given path on behalf
    /// of the given account.
    pub fn create_publish_package(
        &mut self,
        account: &Account,
        path: &Path,
        upgrade_policy: UpgradePolicy,
    ) -> SignedTransaction {
        let package = BuiltPackage::build(MovePackageDir::new(path.to_owned()), true, false)
            .expect("building package must succeed");
        let code = package.extract_code();
        let metadata = package
            .extract_metadata(upgrade_policy)
            .expect("extracting package metdata must succeed");
        self.create_transaction_payload(
            account,
            aptos_stdlib::code_publish_package_txn(
                bcs::to_bytes(&metadata).expect("PackageMetadata has BCS"),
                code,
            ),
        )
    }

    /// Runs transaction which publishes the Move Package.
    pub fn publish_package(
        &mut self,
        account: &Account,
        path: &Path,
        upgrade_policy: UpgradePolicy,
    ) -> TransactionStatus {
        let txn = self.create_publish_package(account, path, upgrade_policy);
        self.run(txn)
    }

    /// Reads the raw, serialized data of a resource.
    pub fn read_resource_raw(
        &self,
        addr: &AccountAddress,
        struct_tag: StructTag,
    ) -> Option<Vec<u8>> {
        let path = access_path::AccessPath::new(
            *addr,
            bcs::to_bytes(&access_path::Path::Resource(struct_tag)).unwrap(),
        );
        self.executor
            .read_from_access_path(&path)
            .and_then(|bytes| if bytes.is_empty() { None } else { Some(bytes) })
    }

    /// Reads the resource data `T`.
    pub fn read_resource<T: DeserializeOwned>(
        &self,
        addr: &AccountAddress,
        struct_tag: StructTag,
    ) -> Option<T> {
        Some(
            bcs::from_bytes::<T>(&self.read_resource_raw(addr, struct_tag)?).expect(
                "serialization expected to succeed (Rust type incompatible with Move type?)",
            ),
        )
    }

    /// Checks whether resource exists.
    pub fn exists_resource(&self, addr: &AccountAddress, struct_tag: StructTag) -> bool {
        self.read_resource_raw(addr, struct_tag).is_some()
    }
}

/// Enables golden files for the given harness. The golden file will be stored side-by-side
/// with the data directory of a Rust source, named after the test function.
#[macro_export]
macro_rules! enable_golden {
    ($h:expr) => {
        $h.internal_set_golden(std::file!(), language_e2e_tests::current_function_name!())
    };
}

impl MoveHarness {
    /// Internal function to support the `enable_golden` macro.
    pub fn internal_set_golden(&mut self, file_macro_value: &str, function_macro_value: &str) {
        // The result of `std::file!` gives us a name relative to the project root,
        // so we need to add that to it. We also want to replace the extension `.rs` with `.data`.
        let mut path = get_project_root().unwrap().join(file_macro_value);
        path.set_extension("data");
        // The result of the `current_function` macro gives us the fully qualified
        // We only want the trailing simple name.
        let fun = function_macro_value.split("::").last().unwrap();
        self.executor
            .set_golden_file_at(&path.display().to_string(), fun)
    }
}

/// Helper to assert transaction is successful
#[macro_export]
macro_rules! assert_success {
    ($s:expr) => {{
        use aptos_types::transaction::*;
        assert_eq!($s, TransactionStatus::Keep(ExecutionStatus::Success))
    }};
}

/// Helper to assert transaction aborts.
#[macro_export]
macro_rules! assert_abort {
    ($s:expr, $c:pat) => {{
        use aptos_types::transaction::*;
        assert!(matches!(
            $s,
            TransactionStatus::Keep(ExecutionStatus::MoveAbort { code: $c, .. })
        ));
    }};
}

/// Helper to assert vm status code.
#[macro_export]
macro_rules! assert_vm_status {
    ($s:expr, $c:pat) => {{
        use aptos_types::transaction::*;
        assert!(matches!(
            $s,
            TransactionStatus::Keep(ExecutionStatus::MiscellaneousError(Some($c)))
        ));
    }};
}
#[macro_use]
extern crate exonum;
extern crate exonum_harness;
extern crate serde;
#[macro_use]
extern crate serde_derive;
extern crate serde_json;

use exonum::blockchain::Service;
use exonum::crypto::{self, HexValue, PublicKey};
use exonum::helpers::Height;
use exonum::messages::Message;
use exonum_harness::{TestHarness, HarnessApi, ComparableSnapshot};

mod counter {
    //! Sample counter service.

    extern crate bodyparser;
    extern crate iron;
    extern crate router;

    use exonum::blockchain::{Blockchain, ApiContext, Service, Transaction};
    use exonum::messages::{RawTransaction, FromRaw, Message};
    use exonum::node::{ApiSender, TransactionSend};
    use exonum::storage::{Fork, Snapshot, Entry};
    use exonum::crypto::{PublicKey, Hash};
    use exonum::encoding;
    use exonum::api::{Api, ApiError};
    use self::iron::Handler;
    use self::iron::prelude::*;
    use self::router::Router;
    use serde_json;

    const SERVICE_ID: u16 = 1;
    const TX_INCREMENT_ID: u16 = 1;

    // "correct horse battery staple" brainwallet pubkey in Ed25519 with SHA-256 digest
    pub const ADMIN_KEY: &'static str = "506f27b1b4c2403f2602d663a059b026\
                                         2afd6a5bcda95a08dd96a4614a89f1b0";

    // // // // Schema // // // //

    pub struct CounterSchema<T> {
        view: T,
    }

    impl<T: AsRef<Snapshot>> CounterSchema<T> {
        pub fn new(view: T) -> Self {
            CounterSchema { view }
        }

        fn entry(&self) -> Entry<&Snapshot, u64> {
            Entry::new(vec![1], self.view.as_ref())
        }

        pub fn count(&self) -> Option<u64> {
            self.entry().get()
        }
    }

    impl<'a> CounterSchema<&'a mut Fork> {
        fn entry_mut(&mut self) -> Entry<&mut Fork, u64> {
            Entry::new(vec![1], self.view)
        }

        fn inc_count(&mut self, inc: u64) -> u64 {
            let count = self.count().unwrap_or(0) + inc;
            self.entry_mut().set(count);
            count
        }

        fn set_count(&mut self, count: u64) {
            self.entry_mut().set(count);
        }
    }

    // // // // Transactions // // // //

    message! {
        struct TxIncrement {
            const TYPE = SERVICE_ID;
            const ID = TX_INCREMENT_ID;
            const SIZE = 40;

            field author: &PublicKey [0 => 32]
            field by: u64 [32 => 40]
        }
    }

    impl Transaction for TxIncrement {
        fn verify(&self) -> bool {
            self.verify_signature(self.author())
        }

        fn execute(&self, fork: &mut Fork) {
            let mut schema = CounterSchema::new(fork);
            schema.inc_count(self.by());
        }
    }

    message! {
        struct TxReset {
            const TYPE = SERVICE_ID;
            const ID = TX_INCREMENT_ID;
            const SIZE = 32;

            field author: &PublicKey [0 => 32]
        }
    }

    impl TxReset {
        pub fn verify_author(&self) -> bool {
            use exonum::crypto::HexValue;
            *self.author() == PublicKey::from_hex(ADMIN_KEY).unwrap()
        }
    }

    impl Transaction for TxReset {
        fn verify(&self) -> bool {
            self.verify_author() && self.verify_signature(self.author())
        }

        fn execute(&self, fork: &mut Fork) {
            let mut schema = CounterSchema::new(fork);
            schema.set_count(0);
        }
    }

    // // // // API // // // //

    #[derive(Serialize, Deserialize)]
    pub struct TransactionResponse {
        pub tx_hash: Hash,
    }

    #[derive(Clone)]
    struct CounterApi {
        channel: ApiSender,
        blockchain: Blockchain,
    }

    impl CounterApi {
        fn increment(&self, req: &mut Request) -> IronResult<Response> {
            match req.get::<bodyparser::Struct<TxIncrement>>() {
                Ok(Some(transaction)) => {
                    let transaction: Box<Transaction> = Box::new(transaction);
                    let tx_hash = transaction.hash();
                    self.channel.send(transaction).map_err(ApiError::from)?;
                    let json = TransactionResponse { tx_hash };
                    self.ok_response(&serde_json::to_value(&json).unwrap())
                }
                Ok(None) => Err(ApiError::IncorrectRequest("Empty request body".into()))?,
                Err(e) => Err(ApiError::IncorrectRequest(Box::new(e)))?,
            }
        }

        fn count(&self) -> Option<u64> {
            let view = self.blockchain.snapshot();
            let schema = CounterSchema::new(&view);
            schema.count()
        }

        fn get_count(&self, _: &mut Request) -> IronResult<Response> {
            let count = self.count().unwrap_or(0);
            self.ok_response(&serde_json::to_value(count).unwrap())
        }

        fn reset(&self, req: &mut Request) -> IronResult<Response> {
            match req.get::<bodyparser::Struct<TxReset>>() {
                Ok(Some(transaction)) => {
                    let transaction: Box<Transaction> = Box::new(transaction);
                    let tx_hash = transaction.hash();
                    self.channel.send(transaction).map_err(ApiError::from)?;
                    let json = TransactionResponse { tx_hash };
                    self.ok_response(&serde_json::to_value(&json).unwrap())
                }
                Ok(None) => Err(ApiError::IncorrectRequest("Empty request body".into()))?,
                Err(e) => Err(ApiError::IncorrectRequest(Box::new(e)))?,
            }
        }

        fn wire_private(&self, router: &mut Router) {
            let self_ = self.clone();
            let reset = move |req: &mut Request| self_.reset(req);
            router.post("/reset", reset, "reset");
        }
    }

    impl Api for CounterApi {
        fn wire(&self, router: &mut Router) {
            let self_ = self.clone();
            let increment = move |req: &mut Request| self_.increment(req);
            router.post("/count", increment, "increment");

            let self_ = self.clone();
            let get_count = move |req: &mut Request| self_.get_count(req);
            router.get("/count", get_count, "get_count");
        }
    }

    // // // // Service // // // //

    pub struct CounterService;

    impl Service for CounterService {
        fn service_name(&self) -> &'static str {
            "counter"
        }

        fn service_id(&self) -> u16 {
            SERVICE_ID
        }

        /// Implement a method to deserialize transactions coming to the node.
        fn tx_from_raw(&self, raw: RawTransaction) -> Result<Box<Transaction>, encoding::Error> {
            let trans: Box<Transaction> = match raw.message_type() {
                TX_INCREMENT_ID => Box::new(TxIncrement::from_raw(raw)?),
                _ => {
                    return Err(encoding::Error::IncorrectMessageType {
                        message_type: raw.message_type(),
                    });
                }
            };
            Ok(trans)
        }

        /// Create a REST `Handler` to process web requests to the node.
        fn public_api_handler(&self, ctx: &ApiContext) -> Option<Box<Handler>> {
            let mut router = Router::new();
            let api = CounterApi {
                channel: ctx.node_channel().clone(),
                blockchain: ctx.blockchain().clone(),
            };
            api.wire(&mut router);
            Some(Box::new(router))
        }

        fn private_api_handler(&self, ctx: &ApiContext) -> Option<Box<Handler>> {
            let mut router = Router::new();
            let api = CounterApi {
                channel: ctx.node_channel().clone(),
                blockchain: ctx.blockchain().clone(),
            };
            api.wire_private(&mut router);
            Some(Box::new(router))
        }
    }
}

use counter::{ADMIN_KEY, CounterService, TxIncrement, TxReset, TransactionResponse, CounterSchema};

fn inc_count(api: &HarnessApi, by: u64) -> TxIncrement {
    let (pubkey, key) = crypto::gen_keypair();
    // Create a presigned transaction
    let tx = TxIncrement::new(&pubkey, by, &key);

    let tx_info: TransactionResponse = api.post("counter", "count", &tx);
    assert_eq!(tx_info.tx_hash, tx.hash());
    tx
}

#[test]
fn test_inc_count() {
    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let mut harness = TestHarness::with_services(services);
    let api = harness.api();
    inc_count(&api, 5);

    harness.create_block();

    // Check that the user indeed is persisted by the service
    let counter: u64 = api.get("counter", "count");
    assert_eq!(counter, 5);
}

#[test]
fn test_inc_count_with_multiple_transactions() {
    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let mut harness = TestHarness::with_services(services);
    let api = harness.api();

    for _ in 0..100 {
        inc_count(&api, 1);
        inc_count(&api, 2);
        inc_count(&api, 3);
        inc_count(&api, 4);

        harness.create_block();
    }

    assert_eq!(harness.state().height(), Height(101));
    let counter: u64 = api.get("counter", "count");
    assert_eq!(counter, 1_000);
}

#[test]
fn test_inc_count_with_manual_tx_control() {
    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let mut harness = TestHarness::with_services(services);
    let api = harness.api();
    let tx_a = inc_count(&api, 5);
    let tx_b = inc_count(&api, 3);

    // Empty block
    harness.create_block_with_transactions(&[]);
    let counter: u64 = api.get("counter", "count");
    assert_eq!(counter, 0);

    harness.create_block_with_transactions(&[tx_b.hash()]);
    let counter: u64 = api.get("counter", "count");
    assert_eq!(counter, 3);

    harness.create_block_with_transactions(&[tx_a.hash()]);
    let counter: u64 = api.get("counter", "count");
    assert_eq!(counter, 8);
}

#[test]
fn test_private_api() {
    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let mut harness = TestHarness::with_services(services);
    let api = harness.api();
    inc_count(&api, 5);
    inc_count(&api, 3);

    harness.create_block();
    let counter: u64 = api.get("counter", "count");
    assert_eq!(counter, 8);

    let (pubkey, key) = crypto::gen_keypair_from_seed(&crypto::Seed::from_slice(
        &crypto::hash(b"correct horse battery staple")[..],
    ).unwrap());
    assert_eq!(pubkey, PublicKey::from_hex(ADMIN_KEY).unwrap());

    let tx = TxReset::new(&pubkey, &key);
    let tx_info: TransactionResponse = api.post_private("counter", "reset", &tx);
    assert_eq!(tx_info.tx_hash, tx.hash());

    harness.create_block();
    let counter: u64 = api.get("counter", "count");
    assert_eq!(counter, 0);
}

#[test]
fn test_probe() {
    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let mut harness = TestHarness::with_services(services);
    let api = harness.api();

    let tx = {
        let (pubkey, key) = crypto::gen_keypair();
        TxIncrement::new(&pubkey, 5, &key)
    };

    let snapshot = harness.probe(tx.clone());
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(5));
    // Verify that the patch has not been applied to the blockchain
    let counter: u64 = api.get("counter", "count");
    assert_eq!(counter, 0);

    let other_tx = {
        let (pubkey, key) = crypto::gen_keypair();
        TxIncrement::new(&pubkey, 3, &key)
    };

    let snapshot = harness.probe_all(vec![Box::new(tx.clone()), Box::new(other_tx.clone())]);
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(8));

    // Posting a transaction is not enough to change the blockchain!
    let _: TransactionResponse = api.post("counter", "count", &tx);
    let snapshot = harness.probe(other_tx.clone());
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(3));

    harness.create_block();
    let snapshot = harness.probe(other_tx.clone());
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(8));
}

#[test]
fn test_duplicate_tx() {
    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let mut harness = TestHarness::with_services(services);
    let api = harness.api();

    let tx = inc_count(&api, 5);
    harness.create_block();
    let _: TransactionResponse = api.post("counter", "count", &tx);
    let _: TransactionResponse = api.post("counter", "count", &tx);
    harness.create_block();
    let counter: u64 = api.get("counter", "count");
    assert_eq!(counter, 5);
}

#[test]
#[should_panic(expected = "Duplicate transactions in probe")]
fn test_probe_duplicate_tx_panic() {
    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let harness = TestHarness::with_services(services);

    let tx = {
        let (pubkey, key) = crypto::gen_keypair();
        TxIncrement::new(&pubkey, 6, &key)
    };
    let snapshot = harness.probe_all(vec![Box::new(tx.clone()), Box::new(tx.clone())]);
    drop(snapshot);
}

#[test]
fn test_probe_advanced() {
    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let mut harness = TestHarness::with_services(services);
    let api = harness.api();

    let tx = {
        let (pubkey, key) = crypto::gen_keypair();
        TxIncrement::new(&pubkey, 6, &key)
    };
    let other_tx = {
        let (pubkey, key) = crypto::gen_keypair();
        TxIncrement::new(&pubkey, 10, &key)
    };
    let admin_tx = {
        let (pubkey, key) = crypto::gen_keypair_from_seed(&crypto::Seed::from_slice(
            &crypto::hash(b"correct horse battery staple")[..],
        ).unwrap());
        assert_eq!(pubkey, PublicKey::from_hex(ADMIN_KEY).unwrap());

        TxReset::new(&pubkey, &key)
    };

    let snapshot = harness.probe(tx.clone());
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(6));
    // Check that data is not persisted
    let snapshot = harness.snapshot();
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), None);

    // Check dependency of the resulting snapshot on tx ordering
    let snapshot = harness.probe_all(vec![Box::new(tx.clone()), Box::new(admin_tx.clone())]);
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(0));
    let snapshot = harness.probe_all(vec![Box::new(admin_tx.clone()), Box::new(tx.clone())]);
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(6));
    // Check that data is (still) not persisted
    let snapshot = harness.snapshot();
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), None);

    api.send(other_tx);
    harness.create_block();
    let snapshot = harness.snapshot();
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(10));

    let snapshot = harness.probe(tx.clone());
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(16));
    // Check that data is not persisted
    let snapshot = harness.snapshot();
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(10));

    // Check dependency of the resulting snapshot on tx ordering
    let snapshot = harness.probe_all(vec![Box::new(tx.clone()), Box::new(admin_tx.clone())]);
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(0));
    let snapshot = harness.probe_all(vec![Box::new(admin_tx.clone()), Box::new(tx.clone())]);
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(6));
    // Check that data is (still) not persisted
    let snapshot = harness.snapshot();
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(10));
}

#[test]
fn test_probe_duplicate_tx() {
    //! Checks that committed transactions do not change the blockchain state when probed.

    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let mut harness = TestHarness::with_services(services);
    let api = harness.api();
    let tx = inc_count(&api, 5);

    let snapshot = harness.probe(tx.clone());
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(5));

    harness.create_block();

    let snapshot = harness.probe(tx.clone());
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(5));

    // Check the mixed case, when some probed transactions are committed and some are not
    let other_tx = inc_count(&api, 7);
    let snapshot = harness.probe_all(vec![Box::new(tx), Box::new(other_tx)]);
    let schema = CounterSchema::new(&snapshot);
    assert_eq!(schema.count(), Some(12));
}

#[test]
fn test_snapshot_comparison() {
    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let mut harness = TestHarness::with_services(services);

    let tx = {
        let (pubkey, key) = crypto::gen_keypair();
        TxIncrement::new(&pubkey, 5, &key)
    };
    harness
        .probe(tx.clone())
        .compare(harness.snapshot())
        .map(CounterSchema::new)
        .map(CounterSchema::count)
        .assert_before("Counter does not exist", Option::is_none)
        .assert_after("Counter has been set", |&c| c == Some(5));

    harness.api().send(tx);
    harness.create_block();

    let other_tx = {
        let (pubkey, key) = crypto::gen_keypair();
        TxIncrement::new(&pubkey, 3, &key)
    };
    harness
        .probe(other_tx.clone())
        .compare(harness.snapshot())
        .map(CounterSchema::new)
        .map(CounterSchema::count)
        .map(|&c| c.unwrap())
        .assert("Counter has increased", |&old, &new| new == old + 3);
}

#[test]
#[should_panic(expected = "Counter has increased")]
fn test_snapshot_comparison_panic() {
    let services: Vec<Box<Service>> = vec![Box::new(CounterService)];
    let mut harness = TestHarness::with_services(services);

    let tx = {
        let (pubkey, key) = crypto::gen_keypair();
        TxIncrement::new(&pubkey, 5, &key)
    };

    harness.api().send(tx.clone());
    harness.create_block();

    // The assertion fails because the transaction is already committed by now
    harness
        .probe(tx.clone())
        .compare(harness.snapshot())
        .map(CounterSchema::new)
        .map(CounterSchema::count)
        .map(|&c| c.unwrap())
        .assert("Counter has increased", |&old, &new| new == old + tx.by());
}

# Generic Factroy Contract

A CosmWasm factory contract, built with [Fadroma](https://crates.io/crates/fadroma), that
is meant to be used as a library. It can be used as a standalone contract or as a part of
a larger contract. It allows to:

 - Make new child instances, list them in pages or get them by contract address.
 - Have extra data that you may want to have the factory store for each instance. By
 default, it only stores the contract address and code hash. Your contract must include
 that in the `InstantiateReplyData` struct that it sets as the response data in its
 instantiate function.
 - Configure whether anyone or just the admin can create child instances (at compile time).
 - Change the child contract code if needed (only the admin address can execute this).
 - Pause or stop the contract if needeed and change the current admin
 (only the admin address can execute these).

 ## Usage
 The `GenericFactory` struct itself has 3 generic parameters:

  - `MSG`: the instantiate message that the child contract expects.
  - `EXTRA`: extra data to be stored for each instance in the factory. This is useful
  to avoid a lot of queries when fetching instances. Otherwise, you'd have to query each
  instance individually for the data. By default, the parameter is set to
  `cosmwasm_std::Empty` i.e no data.
  - `AUTH`: a boolean parameter to indicate whether only the admin is allowed to create
  new instances or anyone is. If set to `true`, it will require admin. By default, the
  parameter is set to `true`.

Use the `instantiate`, `execute`, `query` and `reply` methods on `GenericFactory` to use
the contract as it is. Otherwise, every piece of functionality is exposed as individual
methods which you can use to extend your pre-existing contract.

> The only requirement is that your child contract must set the `InstantiateReplyData`
struct as data in the `cosmwasm_std::Response` object with its own address and the
extra data (if any) to be stored by the factory, in its instantiate function.

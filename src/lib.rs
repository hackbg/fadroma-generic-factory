use std::{
    marker::PhantomData,
    any::type_name
};

use fadroma::{
    schemars::{self, JsonSchema},
    cosmwasm_std::{
        StdResult, Response, Deps, DepsMut, MessageInfo, Env,
        SubMsg, WasmMsg, Coin, Reply, StdError, Empty, Addr,
        CanonicalAddr, SubMsgResponse, SubMsgResult, Binary,
        to_binary, from_binary
    },
    bin_serde::{FadromaSerialize, FadromaDeserialize},
    storage::{SingleItem, TypedKey, map::InsertOnlyMap},
    core::{ContractCode, ContractLink, Humanize, Canonize},
    admin::{self, Admin},
    killswitch::{self, Killswitch},
    namespace
};
use serde::{Serialize, Deserialize, de::DeserializeOwned};

pub const REPLY_ID: u64 = 78024480;
pub const INSTANCE_ADDR_ATTR: &str = "fadroma_instance_address";

pub trait ExtraData: JsonSchema +
    Serialize + DeserializeOwned +
    FadromaSerialize + FadromaDeserialize { }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct InstantiateMsg {
    pub admin: Option<String>,
    pub code: ContractCode
}

#[derive(Serialize, Deserialize, JsonSchema, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg<MSG> {
    CreateInstance(InstanceConfig<MSG>),
    ChangeContractCode(ContractCode),
    Admin(admin::ExecuteMsg),
    Killswitch(killswitch::ExecuteMsg)
}

#[derive(Serialize, Deserialize, JsonSchema, Debug)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    ListInstances { pagination: Pagination },
    InstanceByAddr { addr: String },
    Admin(admin::QueryMsg),
    Killswitch(killswitch::QueryMsg)
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct InstanceConfig<MSG> {
    pub msg: MSG,
    pub funds: Vec<Coin>
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct InstantiateReplyData<
    EXTRA: ExtraData = Empty
> {
    address: Addr,
    #[serde(bound = "")] // See https://github.com/serde-rs/serde/issues/1296
    extra: EXTRA
}

#[derive(Serialize, Deserialize, JsonSchema, FadromaSerialize, FadromaDeserialize, Clone, Debug)]
pub struct Instance<
    A,
    EXTRA: ExtraData
> {
    pub contract: ContractLink<A>,
    #[serde(bound = "")] // See https://github.com/serde-rs/serde/issues/1296
    pub extra: EXTRA
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug)]
pub struct Pagination {
    pub start: u64,
    pub limit: u8
}

#[derive(Serialize, Deserialize, JsonSchema, Debug)]
pub struct PaginatedResponse<T: Serialize> {
    pub entries: Vec<T>,
    pub total: u64
}

pub struct GenericFactory<
    MSG: Serialize,
    EXTRA: ExtraData = Empty,
    const AUTH: bool = true
>{
    msg_phantom: PhantomData<MSG>,
    extra_phantom: PhantomData<EXTRA>
}

namespace!(ContractNs, b"contract");
const CONTRACT: SingleItem<
    ContractCode,
    ContractNs
> = SingleItem::new();

namespace!(InstancesNs, b"instances");

impl<
    MSG: Serialize,
    EXTRA: ExtraData,
    const AUTH: bool
> GenericFactory<MSG, EXTRA, AUTH> {
    pub fn instantiate(
        mut deps: DepsMut,
        _env: Env,
        info: MessageInfo,
        msg: InstantiateMsg
    ) -> StdResult<Response> {
        admin::init(deps.branch(), msg.admin.as_deref(), &info)?;
        CONTRACT.save(deps.storage, &msg.code)?;

        Ok(Response::default())
    }

    pub fn execute(
        deps: DepsMut,
        env: Env,
        info: MessageInfo,
        msg: ExecuteMsg<MSG>
    ) -> StdResult<Response> {
        if !matches!(msg, ExecuteMsg::Killswitch(_)) {
            killswitch::assert_is_operational(deps.as_ref())?;
        }

        match msg {
            ExecuteMsg::CreateInstance(config) =>
                Self::create_instance(deps, env, info, config),
            ExecuteMsg::ChangeContractCode(code) =>
                Self::change_contract_code(deps, info, &code),
            ExecuteMsg::Admin(msg) => match msg {
                admin::ExecuteMsg::ChangeAdmin { mode } =>
                    admin::DefaultImpl::change_admin(
                        deps,
                        env,
                        info,
                        mode
                    )
            }
            ExecuteMsg::Killswitch(msg) => match msg {
                killswitch::ExecuteMsg::SetStatus { status } =>
                    killswitch::DefaultImpl::set_status(
                        deps,
                        env,
                        info,
                        status
                    )
            }
        }
    }

    pub fn query(
        deps: Deps,
        env: Env,
        msg: QueryMsg
    ) -> StdResult<Binary> {
        match msg {
            QueryMsg::ListInstances { pagination } => {
                let result = Self::list_instances(deps, pagination)?;

                to_binary(&result)
            }
            QueryMsg::InstanceByAddr { addr } => {
                let result = Self::instance_by_addr(deps, addr)?;

                to_binary(&result)
            }
            QueryMsg::Admin(msg) => match msg {
                admin::QueryMsg::Admin { } => {
                    let admin = admin::DefaultImpl::admin(deps, env)?;
    
                    to_binary(&admin)
                }
            }
            QueryMsg::Killswitch(msg) => match msg {
                killswitch::QueryMsg::Status { } => {
                    let result = killswitch::DefaultImpl::status(deps, env)?;
    
                    to_binary(&result)
                }
            }
        }
    }

    /// The reply entry point to use if you don't have any custom logic.
    /// If you do, use [`GenericFactory::handle_reply`] which leaves
    /// matching the reply ID and result up to you.
    pub fn reply(
        deps: DepsMut,
        _env: Env,
        reply: Reply
    ) -> StdResult<Response> {
        if reply.id != REPLY_ID {
            return Err(StdError::generic_err(
                format!("Expecting reply with id: {REPLY_ID}.")
            ));
        }

        let response = if let SubMsgResult::Ok(resp) = reply.result {
            let addr = Self::handle_reply(deps, resp)?;

            Response::default()
                .add_attribute_plaintext(INSTANCE_ADDR_ATTR, addr)
        } else {
            Response::default()
        };

        Ok(response)
    }

    /// Lower level function to use when you have additional logic
    /// in your reply handler. Otherwise, use [`GenericFactory::reply`].
    /// You should match the ID of the reply with [`REPLY_ID`] and then
    /// call this function. Returns the address of the new instance.
    pub fn handle_reply(deps: DepsMut, resp: SubMsgResponse) -> StdResult<Addr> {
        let Some(data) = resp.data else {
            return Err(StdError::generic_err(format!(
                "Expecting non-empty data in reply of type {}.",
                type_name::<InstantiateReplyData<EXTRA>>()
            )));
        };

        let data: InstantiateReplyData<EXTRA> = from_binary(&data)?;

        let contract = CONTRACT.load_or_error(deps.storage)?;
        let mut instances = Self::instances();

        let address = data.address.as_ref().canonize(deps.api)?;
        let key = address.clone(); // it is what it is...
        
        instances.insert(
            deps.storage,
            &key,
            &Instance {
                contract: ContractLink {
                    address,
                    code_hash: contract.code_hash
                },
                extra: data.extra
            }
        )?;

        Ok(data.address)
    }

    pub fn create_instance(
        deps: DepsMut,
        env: Env,
        info: MessageInfo,
        config: InstanceConfig<MSG>
    ) -> StdResult<Response> {
        if AUTH {
            admin::assert(deps.as_ref(), &info)?;
        }

        let contract = CONTRACT.load_or_error(deps.storage)?;
        let label = format!(
            "Fadroma factory child instance created at: {}",
            env.block.time.seconds()
        );
    
        let msg = SubMsg::reply_on_success(
            WasmMsg::Instantiate {
                code_id: contract.id,
                code_hash: contract.code_hash,
                msg: to_binary(&config.msg)?,
                funds: config.funds,
                label
            },
            REPLY_ID
        );
    
        Ok(Response::default().add_submessage(msg))
    }

    #[admin::require_admin]
    pub fn change_contract_code(
        deps: DepsMut,
        info: MessageInfo,
        code: &ContractCode
    ) -> StdResult<Response> {
        CONTRACT.save(deps.storage, code)?;

        Ok(Response::default())
    }

    pub fn list_instances(deps: Deps, pagination: Pagination) ->
        StdResult<PaginatedResponse<Instance<Addr, EXTRA>>>
    {
        let limit = pagination.limit.min(Pagination::MAX_LIMIT);

        let instances = Self::instances();
        let iter = instances.values(deps.storage)?;
        let total = iter.len();

        let iter = iter
            .skip(pagination.start as usize)
            .take(limit as usize);

        let mut entries = Vec::with_capacity(iter.len());
        for instance in iter {
            let instance = instance?;

            entries.push(Instance {
                contract: instance.contract.humanize(deps.api)?,
                extra: instance.extra
            });
        }

        Ok(PaginatedResponse {
            total,
            entries
        })
    }

    pub fn instance_by_addr(deps: Deps, addr: String) ->
        StdResult<Option<Instance<Addr, EXTRA>>>
    {
        let addr = addr.as_str().canonize(deps.api)?;

        let instances = Self::instances();
        let Some(instance) = instances.get(deps.storage, &addr)? else {
            return Ok(None);
        };

        Ok(Some(Instance {
            contract: instance.contract.humanize(deps.api)?,
            extra: instance.extra
        }))
    }

    #[inline]
    fn instances<'a>() -> InsertOnlyMap<
        TypedKey<'a, CanonicalAddr>,
        Instance<CanonicalAddr, EXTRA>,
        InstancesNs
    > {
        InsertOnlyMap::new()
    }
}

impl Pagination {
    pub const MAX_LIMIT: u8 = 30;

    #[inline]
    pub fn new(start: u64, limit: u8) -> Self {
        Self { start, limit}
    }
}

impl InstantiateReplyData<Empty> {
    #[inline]
    pub fn new(address: Addr) -> Self {
        Self {
            address,
            extra: Empty { }
        }
    }
}

impl<EXTRA: ExtraData> InstantiateReplyData<EXTRA> {
    #[inline]
    pub fn with_extra(address: Addr, extra: EXTRA) -> Self {
        Self {
            address,
            extra
        }
    }
}

impl<T: JsonSchema +
    Serialize + DeserializeOwned +
    FadromaSerialize + FadromaDeserialize
> ExtraData for T { }

#[cfg(test)]
mod tests {
    use super::*;
    use fadroma::{
        core::ContractLink,
        ensemble::{
            ContractEnsemble, ContractHarness, AnyResult, MockEnv,
            ResponseVariants, ExecuteResponse
        }
    };

    const ADMIN: &str = "admin";

    impl<
        MSG: Serialize + DeserializeOwned,
        EXTRA: ExtraData,
        const AUTH: bool
    > ContractHarness for GenericFactory<MSG, EXTRA, AUTH> {
        fn instantiate(
            &self,
            deps: DepsMut,
            env: Env,
            info: MessageInfo,
            msg: Binary
        ) -> AnyResult<Response> {
            let result = Self::instantiate(deps, env, info, from_binary(&msg)?)?;

            Ok(result)
        }

        fn execute(
            &self,
            deps: DepsMut,
            env: Env,
            info: MessageInfo,
            msg: Binary
        ) -> AnyResult<Response> {
            let result = Self::execute(deps, env, info, from_binary(&msg)?)?;

            Ok(result)
        }

        fn query(&self, deps: Deps, env: Env, msg: Binary) -> AnyResult<Binary> {
            let result = Self::query(deps, env, from_binary(&msg)?)?;

            Ok(result)
        }

        fn reply(&self, deps: DepsMut, env: Env, reply: Reply) -> AnyResult<Response> {
            let result = Self::reply(deps, env, reply)?;

            Ok(result)
        }
    }

    struct Child;

    #[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
    pub struct ChildInstantiateMsg {
        text: String
    }

    impl ContractHarness for Child {
        fn instantiate(
            &self,
            _deps: DepsMut,
            env: Env,
            _info: MessageInfo,
            msg: Binary
        ) -> AnyResult<Response> {
            let msg: ChildInstantiateMsg = from_binary(&msg)?;

            Ok(Response::new()
                .set_data(to_binary(&InstantiateReplyData {
                    address: env.contract.address,
                    extra: msg.text
                })?)
            )
        }

        fn execute(
            &self,
            _deps: DepsMut,
            _env: Env,
            _info: MessageInfo,
            _msg: Binary
        ) -> AnyResult<Response> {
            todo!()
        }

        fn query(&self, _deps: Deps, _env: Env, _msg: Binary) -> AnyResult<Binary> {
            todo!()
        }
    }

    struct Suite {
        ensemble: ContractEnsemble,
        factory: ContractLink<Addr>
    }

    impl Suite {
        fn new<const AUTH: bool>() -> Self {
            let mut ensemble = ContractEnsemble::new();
            let child = ensemble.register(Box::new(Child));
            let factory = ensemble.register(
                Box::new(GenericFactory::<ChildInstantiateMsg, String, AUTH> {
                    msg_phantom: PhantomData,
                    extra_phantom: PhantomData
                })
            );

            let factory = ensemble.instantiate(
                factory.id,
                &InstantiateMsg {
                    admin: None,
                    code: child
                },
                MockEnv::new(ADMIN, "factory")
            )
            .unwrap()
            .instance;

            Self { ensemble, factory }
        }
    }

    #[test]
    fn only_admin_can_instantiate_when_auth_param_is_true() {
        let Suite { mut ensemble, factory } = Suite::new::<true>();

        let config = InstanceConfig {
            msg: ChildInstantiateMsg {
                text: String::from("flaming swords")
            },
            funds: Vec::new()
        };
        
        let err = ensemble.execute(
            &ExecuteMsg::CreateInstance(config.clone()),
            MockEnv::new("not admin", &factory.address)
        ).unwrap_err();

        assert_eq!(
            err.unwrap_contract_error().to_string(),
            "Generic error: Unauthorized"
        );

        ensemble.execute(
            &ExecuteMsg::CreateInstance(config),
            MockEnv::new(ADMIN, &factory.address)
        ).unwrap();
    }

    #[test]
    fn only_admin_can_instantiate_when_auth_param_is_false() {
        let Suite { mut ensemble, factory } = Suite::new::<false>();

        let config = InstanceConfig {
            msg: ChildInstantiateMsg {
                text: String::from("flaming swords")
            },
            funds: Vec::new()
        };

        ensemble.execute(
            &ExecuteMsg::CreateInstance(config.clone()),
            MockEnv::new("not admin", &factory.address)
        ).unwrap();

        ensemble.execute(
            &ExecuteMsg::CreateInstance(config),
            MockEnv::new(ADMIN, &factory.address)
        ).unwrap();
    }

    #[test]
    fn instances_are_stored_with_extra_data() {
        let Suite { mut ensemble, factory } = Suite::new::<false>();

        let config = InstanceConfig {
            msg: ChildInstantiateMsg {
                text: String::from("flaming swords")
            },
            funds: Vec::new()
        };

        let resp = ensemble.execute(
            &ExecuteMsg::CreateInstance(config.clone()),
            MockEnv::new("not admin", &factory.address)
        ).unwrap();

        let addr = extract_instance_addr(&resp);

        let instance: Instance<Addr, String> = ensemble.query(
            &factory.address,
            &QueryMsg::InstanceByAddr { addr }
        )
        .unwrap();

        assert!(instance.contract.address.as_str().starts_with("fadroma factory child instance"));
        assert_eq!(instance.contract.code_hash, "test_contract_0");
        assert_eq!(instance.extra, "flaming swords");

        let instance: Option<Instance<Addr, String>> = ensemble.query(
            &factory.address,
            &QueryMsg::InstanceByAddr { addr: "wrong addr".into() }
        )
        .unwrap();

        assert!(instance.is_none());
    }

    #[test]
    fn list_instances() {
        let Suite { mut ensemble, factory } = Suite::new::<true>();

        let num_instances: u8 = 10;

        for i in 0..num_instances {
            let config = InstanceConfig {
                msg: ChildInstantiateMsg {
                    text: format!("extra data {i}")
                },
                funds: Vec::new()
            };

            ensemble.execute(
                &ExecuteMsg::CreateInstance(config),
                MockEnv::new(ADMIN, &factory.address)
            ).unwrap();
        }

        let instances: PaginatedResponse<Instance<Addr, String>> = ensemble.query(
            &factory.address,
            &QueryMsg::ListInstances {
                pagination: Pagination::new(0, num_instances / 2)
            }
        ).unwrap();

        assert_eq!(instances.total, num_instances as u64);
        assert_eq!(instances.entries.len(), (num_instances / 2) as usize);

        for (i, instance) in instances.entries.iter().enumerate() {
            assert!(instance.contract.address.as_str().starts_with("fadroma factory child instance"));
            assert_eq!(instance.contract.code_hash, "test_contract_0");
            assert_eq!(instance.extra, format!("extra data {i}"));
        }

        let instances: PaginatedResponse<Instance<Addr, String>> = ensemble.query(
            &factory.address,
            &QueryMsg::ListInstances {
                pagination: Pagination::new((num_instances / 2) as u64, num_instances)
            }
        ).unwrap();

        assert_eq!(instances.total, num_instances as u64);
        assert_eq!(instances.entries.len(), (num_instances / 2) as usize);

        for (i, instance) in instances.entries.iter().enumerate() {
            assert!(instance.contract.address.as_str().starts_with("fadroma factory child instance"));
            assert_eq!(instance.contract.code_hash, "test_contract_0");
            assert_eq!(instance.extra, format!("extra data {}", i as u8 + (num_instances / 2)));
        }
    }

    #[test]
    fn only_admin_can_change_contract_code() {
        let Suite { mut ensemble, factory } = Suite::new::<false>();
        let err = ensemble.execute(
            &ExecuteMsg::<ChildInstantiateMsg>::ChangeContractCode(
                ContractCode {
                    id: 2,
                    code_hash: "code_hash".into()
                }
            ),
            MockEnv::new("not admin", &factory.address)
        ).unwrap_err();

        assert_eq!(
            err.unwrap_contract_error().to_string(),
            "Generic error: Unauthorized"
        );

        ensemble.execute(
            &ExecuteMsg::<ChildInstantiateMsg>::ChangeContractCode(
                ContractCode {
                    id: 2,
                    code_hash: "code_hash".into()
                }
            ),
            MockEnv::new(ADMIN, factory.address)
        ).unwrap();
    }

    fn extract_instance_addr(resp: &ExecuteResponse) -> String {
        let resp = resp.iter().find(|x| x.is_reply()).expect("no reply response");

        if let ResponseVariants::Reply(reply) = resp {
            let addr = reply.response.attributes.iter()
                .find(|x| x.key == INSTANCE_ADDR_ATTR);

            if let Some(addr) = addr {
                return addr.value.clone();
            }
        };
        
        panic!("Couldn't find the {}", INSTANCE_ADDR_ATTR);
    }
}

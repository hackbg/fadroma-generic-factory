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
    admin,
    namespace
};
use serde::{Serialize, Deserialize, de::DeserializeOwned};

pub const REPLY_ID: u64 = 78024480;

pub trait ExtraData: JsonSchema +
    Serialize + DeserializeOwned +
    FadromaSerialize + FadromaDeserialize { }

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct InstantiateMsg {
    admin: Option<String>,
    code: ContractCode
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg<MSG> {
    CreateInstance(InstanceConfig<MSG>)
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    ListInstances { pagination: Pagination },
    InstanceByAddr { addr: String }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
pub struct InstanceConfig<MSG> {
    msg: MSG,
    funds: Vec<Coin>
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
    contract: ContractLink<A>,
    #[serde(bound = "")] // See https://github.com/serde-rs/serde/issues/1296
    extra: EXTRA
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
    MSG: Serialize + DeserializeOwned,
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
    MSG: Serialize + DeserializeOwned,
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
        match msg {
            ExecuteMsg::CreateInstance(config) =>
                Self::create_instance(deps, env, info, config)
        }
    }

    pub fn query(
        deps: Deps,
        _env: Env,
        _info: MessageInfo,
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

        if let SubMsgResult::Ok(resp) = reply.result {
            Self::handle_reply(deps, resp)?;
        }

        Ok(Response::default())
    }

    /// Lower level function to use when you have additional logic
    /// in your reply handler. Otherwise, use [`GenericFactory::reply`].
    /// You should match the ID of the reply with [`REPLY_ID`] and then
    /// call this function.
    pub fn handle_reply(deps: DepsMut, resp: SubMsgResponse) -> StdResult<()> {
        let Some(data) = resp.data else {
            return Err(StdError::generic_err(format!(
                "Expecting non-empty data in reply of type {}.",
                type_name::<InstantiateReplyData<EXTRA>>()
            )));
        };

        let data: InstantiateReplyData<EXTRA> = from_binary(&data)?;

        let contract = CONTRACT.load_or_error(deps.storage)?;
        let mut instances = Self::instances();

        let address = data.address.canonize(deps.api)?;
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

        Ok(())
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
}

impl<T: JsonSchema +
    Serialize + DeserializeOwned +
    FadromaSerialize + FadromaDeserialize
> ExtraData for T { }


//! Integration test: every type that appears in the socket API surface must be
//! reachable from outside the crate. This file is compiled as a separate crate,
//! so any type that can't be named here isn't properly exported.
//! Compilation failure == missing export.

use gnosis_vpn_lib::balance::{BalanceRecommendation, CapacityAllocator, CapacityEntry, Capacity, FundingIssue};
use gnosis_vpn_lib::command::{
    ActiveSession, BalanceResponse, ChannelBalance, ChannelOut, Command, ConnStats, ConnectResponse,
    ConnectingInfo, ConnectedInfo, DestinationState, DisconnectResponse, DisconnectingInfo,
    FundingToolResponse, HoprInitStatus, HoprStatus, Info, InfoResponse, NerdStatsResponse,
    ReconnectingInfo, Response, RouteHealthView, RunMode, StartClientResponse, StopClientResponse,
    StatusResponse, TicketStats, TicketStatsStatus, WorkerCommand,
};
use gnosis_vpn_lib::connection::destination::{Address, Destination, HopRouting};
use gnosis_vpn_lib::connection::down::Phase as DownPhase;
use gnosis_vpn_lib::connection::up::Phase as UpPhase;
use gnosis_vpn_lib::route_health::{
    ExitHealth, Health, LoadAvg, RouteHealthState, Slots, UnrecoverableReason, Versions,
};

// This function exists only to force the compiler to verify that every type in
// the socket API surface is reachable from outside the crate. It is never called.
#[allow(dead_code)]
fn assert_types_are_accessible() {
    let _: Command;
    let _: WorkerCommand;
    let _: Response;
    let _: StatusResponse;
    let _: ConnectingInfo;
    let _: ReconnectingInfo;
    let _: ConnectedInfo;
    let _: DisconnectingInfo;
    let _: DestinationState;
    let _: RunMode;
    let _: HoprStatus;
    let _: HoprInitStatus;
    let _: InfoResponse;
    let _: StartClientResponse;
    let _: StopClientResponse;
    let _: ConnectResponse;
    let _: DisconnectResponse;
    let _: FundingToolResponse;
    let _: RouteHealthView;
    let _: TicketStatsStatus;
    let _: TicketStats;
    let _: NerdStatsResponse;
    let _: ConnStats;
    let _: ActiveSession;
    let _: BalanceResponse;
    let _: ChannelOut;
    let _: ChannelBalance;
    let _: Info;
    let _: Destination;
    let _: Address;
    let _: HopRouting;
    let _: UpPhase;
    let _: DownPhase;
    let _: RouteHealthState;
    let _: UnrecoverableReason;
    let _: ExitHealth;
    let _: Versions;
    let _: Health;
    let _: Slots;
    let _: LoadAvg;
    let _: BalanceRecommendation;
    let _: FundingIssue;
    let _: CapacityEntry;
    let _: CapacityAllocator;
    let _: Capacity;
}

#[test]
fn socket_types_compile() {}

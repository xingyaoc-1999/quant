use hypersdk::{
    hypercore::{self, PrivateKeySigner},
    Address,
};

const TEST_PRIVATE_KEY: &str = "0x64a7e2d8f62268f0f2456509248714a9d70f2c90d4888afff0300a1d45267ee2";

const TEST_WALLET_ADDRESS: &str = "0x8B2019cCCCeF2A20314DC15cf4afE7d9fFB6aE14";

fn setup() -> (hypercore::HttpClient, PrivateKeySigner) {
    let client = hypercore::mainnet();
    let signer: PrivateKeySigner = TEST_PRIVATE_KEY.parse().expect("Invalid key!");
    (client, signer)
}

#[tokio::test]
async fn test_query_positions() -> anyhow::Result<()> {
    let client = hypercore::mainnet();
    let user: Address = "0x8B2019cCCCeF2A20314DC15cf4afE7d9fFB6aE14".parse()?;

    let balances = client.user_balances(user).await?;
    println!("=== 账户余额 ===");
    for b in balances {
        println!("{}: total={}, held={}", b.coin, b.total, b.hold);
    }

    let state = client.clearinghouse_state(user, None).await?;
    println!("=== 当前持仓 ===");
    for asset_position in &state.asset_positions {
        let pos = &asset_position.position;
        println!(
            "{} {}: {} @ {:?} (PnL: {})",
            pos.side(),
            pos.coin,
            pos.szi,
            pos.entry_px,
            pos.unrealized_pnl
        );
    }

    Ok(())
}

#[tokio::test]
async fn test_get_markets() -> anyhow::Result<()> {
    let (client, _) = setup();
    let perps = client.perps().await?;
    println!("=== 永续合约市场 ===");
    for (i, m) in perps.iter().enumerate() {
        println!("索引 {}: {} (最大杠杆 {}x)", i, m.name, m.max_leverage);
    }
    Ok(())
}

// /// 3. 市价单测试 (立即开仓)
// #[tokio::test]
// async fn test_market_order() -> anyhow::Result<()> {
//     let (client, signer) = setup();
//     let asset_id = 0; // 通常 0 是 BTC，请先运行 test_get_markets 确认

//     // 市价买单：0.001 张合约
//     let order = OrderRequest {
//         asset: asset_id,
//         is_buy: true,
//         reduce_only: false,
//         limit_px: Decimal::ZERO, // 市价单可设为0
//         sz: Decimal::from_str("0.001")?,
//         order_type: OrderType::Market,
//         ..Default::default()
//     };

//     let response = client.place_order(order, &signer).await?;
//     println!("市价单结果: {:?}", response);
//     Ok(())
// }

// /// 4. 限价单测试
// #[tokio::test]
// async fn test_limit_order() -> anyhow::Result<()> {
//     let (client, signer) = setup();
//     let asset_id = 0; // BTC

//     let current_price = 60000.0; // 请替换为实际价格或从行情获取

//     let order = OrderRequest {
//         asset: asset_id,
//         is_buy: true,
//         reduce_only: false,
//         limit_px: Decimal::from_str(&format!("{:.2}", current_price * 0.99))?, // 低于现价1%的限价买单
//         sz: Decimal::from_str("0.001")?,
//         order_type: OrderType::Limit(TimeInForce::Gtc),
//         ..Default::default()
//     };

//     let response = client.place_order(order, &signer).await?;
//     println!("限价单结果: {:?}", response);
//     Ok(())
// }

// /// 5. 止损单 (使用 Trigger 订单模拟 Stop Market)
// #[tokio::test]
// async fn test_stop_loss_order() -> anyhow::Result<()> {
//     let (client, signer) = setup();
//     let asset_id = 0; // BTC

//     // 假设我们持有多头仓位，想在 59000 止损 (Trigger 订单)
//     let stop_price = 59000.0;

//     let order = OrderRequest {
//         asset: asset_id,
//         is_buy: false,                   // 平多
//         reduce_only: true,               // 仅减仓
//         limit_px: Decimal::ZERO,         // 市价触发
//         sz: Decimal::from_str("0.001")?, // 数量，必须与持仓匹配
//         order_type: OrderType::Trigger(TriggerOrder {
//             trigger_px: Decimal::from_str(&format!("{:.2}", stop_price))?,
//             is_market: true, // 市价触发
//             tpsl: Tpsl::Sl,  // 止损类型
//         }),
//         ..Default::default()
//     };

//     let response = client.place_order(order, &signer).await?;
//     println!("止损单结果: {:?}", response);
//     Ok(())
// }

// /// 6. 止盈单 (使用 Trigger 订单模拟 Take Profit Limit)
// #[tokio::test]
// async fn test_take_profit_order() -> anyhow::Result<()> {
//     let (client, signer) = setup();
//     let asset_id = 0; // BTC

//     let tp_trigger = 61000.0;
//     let tp_limit = 61200.0; // 限价，可略高于触发价

//     let order = OrderRequest {
//         asset: asset_id,
//         is_buy: false, // 平多
//         reduce_only: true,
//         limit_px: Decimal::from_str(&format!("{:.2}", tp_limit))?,
//         sz: Decimal::from_str("0.001")?,
//         order_type: OrderType::Trigger(TriggerOrder {
//             trigger_px: Decimal::from_str(&format!("{:.2}", tp_trigger))?,
//             is_market: false, // 限价触发
//             tpsl: Tpsl::Tp,   // 止盈类型
//         }),
//         ..Default::default()
//     };

//     let response = client.place_order(order, &signer).await?;
//     println!("止盈单结果: {:?}", response);
//     Ok(())
// }

// /// 7. 取消所有挂单
// #[tokio::test]
// async fn test_cancel_all_orders() -> anyhow::Result<()> {
//     let (client, signer) = setup();
//     let asset_id = 0;

//     // 取消所有该资产的挂单
//     let response = client.cancel_all_orders(asset_id, &signer).await?;
//     println!("取消结果: {:?}", response);
//     Ok(())
// }

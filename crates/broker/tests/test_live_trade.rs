use binance_sdk::config::ConfigurationRestApi;
use binance_sdk::derivatives_trading_usds_futures::rest_api::{
    AccountInformationV2Params, RestApi,
};
fn create_client() -> RestApi {
    let api_key = "2uaC883ZX0gzMfAGavQzP3T0b5kCDHYQ4X7Kkc5fXycJRkzbcXk0q05Y3Kg2OEHN";
    let secret_key = "jzMSDp1azQHqDyOvXlF65ldB4pRaZ1mu773xUtEIQApktHxS4CEuctYTmzn3Gdda";

    let rest_conf = ConfigurationRestApi::builder()
        .api_key(api_key)
        .api_secret(secret_key)
        .base_path("https://demo-fapi.binance.com")
        .build()
        .expect("Failed to build REST configuration");
    RestApi::new(rest_conf)
}

#[tokio::test]
async fn test_connection_and_balance() {
    let client = create_client();
    let account = client
        .account_information_v2(AccountInformationV2Params::builder().build().unwrap())
        .await
        .expect("Failed to query account")
        .data()
        .await
        .expect("Failed to parse account data");

    // 打印 USDT 余额（如果有）
}

// 其他测试（开仓/止损/止盈）保持不变，可按需取消注释

use std::collections::{BTreeMap, VecDeque};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use longport::blocking::{PortfolioContextSync, QuoteContextSync, TradeContextSync};
use longport::portfolio::ExchangeRates;
use longport::quote::{AdjustType, Period, TradeSessions};
use longport::trade::{
    GetStockPositionsOptions, GetTodayOrdersOptions, OrderSide as LongportOrderSide,
    OrderStatus as LongportOrderStatus, OrderType as LongportOrderType, SubmitOrderOptions,
    TimeInForceType,
};
use longport::{Config, Decimal};

use crate::runtime::{
    AccountSummary, AuditSink, BookLevel, BrokerAdapter, CancelOrderRequest, Candle,
    MarketDataAdapter, MarketDataResult, Order, OrderBook, OrderDecision, OrderRequest, OrderSide,
    OrderType, PaperAccountSnapshot, PortfolioSnapshot, Position, Quote, RiskLimits, RiskPolicy,
};

const SOURCE: &str = "longport-openapi";
const TRADE_WINDOW: Duration = Duration::from_secs(30);
const TRADE_MIN_INTERVAL: Duration = Duration::from_millis(20);
const TRADE_WINDOW_REQUESTS: usize = 30;

pub(crate) enum LongportAuth {
    ApiKey {
        app_key: String,
        app_secret: String,
        access_token: String,
    },
    OAuth {
        client_id: String,
    },
}

/// Longbridge/LongPort OpenAPI adapter shared by quote and trade runtimes.
///
/// One instance owns both official SDK contexts. It implements the two neenee
/// adapter contracts so a configured runtime does not establish duplicate
/// quote or trade sessions.
pub(crate) struct LongportAdapter {
    client: Arc<dyn LongportClient>,
    risk: Arc<dyn RiskPolicy>,
    audit: Arc<dyn AuditSink>,
    order_history: Mutex<Vec<OrderDecision>>,
    next_decision_id: AtomicU64,
}

impl LongportAdapter {
    pub(crate) fn connect(
        auth: LongportAuth,
        account_currency: Option<String>,
        risk: Arc<dyn RiskPolicy>,
        audit: Arc<dyn AuditSink>,
    ) -> Result<Self, String> {
        let config = match auth {
            LongportAuth::ApiKey {
                app_key,
                app_secret,
                access_token,
            } => Config::from_apikey(app_key, app_secret, access_token),
            LongportAuth::OAuth { client_id } => {
                let oauth = longport::oauth::OAuthBuilder::new(client_id)
                    .build_blocking(|url| eprintln!("Authorize LongPort OpenAPI at: {url}"))
                    .map_err(|e| format!("LongPort OAuth authorization failed: {e}"))?;
                Config::from_oauth(oauth)
            }
        };
        let client = Arc::new(SdkLongportClient::new(Arc::new(config), account_currency)?);
        Ok(Self::with_client(client, risk, audit))
    }

    fn with_client(
        client: Arc<dyn LongportClient>,
        risk: Arc<dyn RiskPolicy>,
        audit: Arc<dyn AuditSink>,
    ) -> Self {
        Self {
            client,
            risk,
            audit,
            order_history: Mutex::new(Vec::new()),
            next_decision_id: AtomicU64::new(0),
        }
    }

    fn decision_id(&self) -> String {
        let id = self.next_decision_id.fetch_add(1, Ordering::Relaxed);
        format!("LONGPORT-DECISION-{id:06}")
    }

    fn live_portfolio(&self, symbol: Option<&str>) -> MarketDataResult<PortfolioSnapshot> {
        let mut portfolio = self.client.portfolio(symbol)?;
        portfolio.risk_limits = self.risk.limits();
        portfolio.order_history = self
            .order_history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Ok(portfolio)
    }

    fn unavailable_account(&self) -> AccountSummary {
        AccountSummary {
            currency: "longport".to_string(),
            cash: 0.0,
            available_cash: 0.0,
            equity: 0.0,
            realized_pnl: 0.0,
            total_commission: 0.0,
            net_pnl: 0.0,
            gross_exposure: 0.0,
            projected_gross_exposure: 0.0,
            reserved_buy_notional: 0.0,
            reserved_buy_commission: 0.0,
            reserved_sell_notional: 0.0,
            buying_power: 0.0,
        }
    }

    fn unavailable_portfolio(&self) -> PortfolioSnapshot {
        PortfolioSnapshot {
            positions: Vec::new(),
            open_orders: Vec::new(),
            account_mode: "longport-live-unavailable".to_string(),
            account: self.unavailable_account(),
            risk_limits: self.risk.limits(),
            order_history: self
                .order_history
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    fn finalize(&self, mut decision: OrderDecision) -> OrderDecision {
        if let Err(err) = self.audit.record(&decision) {
            decision.audit_error = Some(err);
        }
        let mut history = self.order_history.lock().unwrap_or_else(|e| e.into_inner());
        history.push(decision.clone());
        if history.len() > 1_000 {
            history.remove(0);
        }
        decision
    }

    fn failed_decision(
        &self,
        decision_id: String,
        status: &str,
        reason: String,
        account: AccountSummary,
    ) -> OrderDecision {
        self.finalize(OrderDecision {
            decision_id,
            status: status.to_string(),
            order: None,
            rejection_reason: Some(reason),
            risk_checks: Vec::new(),
            account,
            audit_error: None,
            persistence_error: None,
        })
    }
}

impl MarketDataAdapter for LongportAdapter {
    fn quote(&self, symbol: &str) -> MarketDataResult<Quote> {
        self.client.quote(symbol)
    }

    fn candles(&self, symbol: &str, interval: &str, limit: usize) -> MarketDataResult<Vec<Candle>> {
        self.client.candles(symbol, interval, limit)
    }

    fn depth(&self, symbol: &str, limit: usize) -> MarketDataResult<OrderBook> {
        self.client.depth(symbol, limit)
    }
}

impl BrokerAdapter for LongportAdapter {
    fn place_order(&self, req: OrderRequest, quote: Quote) -> OrderDecision {
        let decision_id = self.decision_id();
        let portfolio = match self.live_portfolio(None) {
            Ok(portfolio) => portfolio,
            Err(err) => {
                return self.failed_decision(
                    decision_id,
                    "broker_error",
                    format!("LongPort portfolio preflight failed: {err}"),
                    self.unavailable_account(),
                );
            }
        };
        let risk_price = match order_risk_price(&req, &quote)
            .and_then(|price| self.client.account_value(&req.symbol, price))
        {
            Ok(price) => price,
            Err(err) => {
                return self.failed_decision(
                    decision_id,
                    "rejected_invalid",
                    err,
                    portfolio.account,
                );
            }
        };
        let account_snapshot = PaperAccountSnapshot {
            cash: portfolio.account.cash,
            positions: portfolio.positions.clone(),
            open_orders: portfolio.open_orders.clone(),
            gross_exposure: portfolio.account.gross_exposure,
        };
        let assessment = self.risk.assess(&account_snapshot, &req, risk_price, 0.0);
        if let Some(reason) = assessment.rejection_reason {
            return self.finalize(OrderDecision {
                decision_id,
                status: "rejected_risk".to_string(),
                order: None,
                rejection_reason: Some(reason),
                risk_checks: assessment.checks,
                account: portfolio.account,
                audit_error: None,
                persistence_error: None,
            });
        }

        let order_id = match self.client.submit_order(&req) {
            Ok(order_id) => order_id,
            Err(err) => {
                return self.finalize(OrderDecision {
                    decision_id,
                    status: "broker_error".to_string(),
                    order: None,
                    rejection_reason: Some(format!("LongPort submit order failed: {err}")),
                    risk_checks: assessment.checks,
                    account: portfolio.account,
                    audit_error: None,
                    persistence_error: None,
                });
            }
        };
        let order = Order {
            order_id,
            status: "submitted_longport".to_string(),
            symbol: normalize_symbol(&req.symbol),
            side: req.side,
            order_type: req.order_type,
            quantity: req.quantity,
            limit_price: req.price,
            fill_price: None,
            filled_quantity: 0.0,
            commission: 0.0,
            timestamp_ms: now_ms(),
        };
        self.finalize(OrderDecision {
            decision_id,
            status: order.status.clone(),
            order: Some(order),
            rejection_reason: None,
            risk_checks: assessment.checks,
            account: portfolio.account,
            audit_error: None,
            persistence_error: None,
        })
    }

    fn cancel_order(&self, req: CancelOrderRequest) -> OrderDecision {
        let decision_id = self.decision_id();
        let (order, account) = self
            .live_portfolio(None)
            .map(|portfolio| {
                let order = portfolio
                    .open_orders
                    .iter()
                    .find(|order| order.order_id == req.order_id)
                    .cloned();
                (order, portfolio.account)
            })
            .unwrap_or_else(|_| (None, self.unavailable_account()));
        match self.client.cancel_order(&req.order_id) {
            Ok(()) => {
                let mut order = order;
                if let Some(order) = &mut order {
                    order.status = "cancelled_longport".to_string();
                }
                self.finalize(OrderDecision {
                    decision_id,
                    status: "cancelled_longport".to_string(),
                    order,
                    rejection_reason: None,
                    risk_checks: Vec::new(),
                    account,
                    audit_error: None,
                    persistence_error: None,
                })
            }
            Err(err) => self.failed_decision(
                decision_id,
                "broker_error",
                format!("LongPort cancel order failed: {err}"),
                account,
            ),
        }
    }

    fn apply_quote(&self, _quote: Quote) -> Vec<OrderDecision> {
        Vec::new()
    }

    fn portfolio(&self, symbol: Option<&str>) -> PortfolioSnapshot {
        self.live_portfolio(symbol)
            .unwrap_or_else(|_| self.unavailable_portfolio())
    }

    fn try_portfolio(&self, symbol: Option<&str>) -> MarketDataResult<PortfolioSnapshot> {
        self.live_portfolio(symbol)
    }
}

trait LongportClient: Send + Sync {
    fn quote(&self, symbol: &str) -> MarketDataResult<Quote>;
    fn candles(&self, symbol: &str, interval: &str, limit: usize) -> MarketDataResult<Vec<Candle>>;
    fn depth(&self, symbol: &str, limit: usize) -> MarketDataResult<OrderBook>;
    fn portfolio(&self, symbol: Option<&str>) -> MarketDataResult<PortfolioSnapshot>;
    fn account_value(&self, symbol: &str, value: f64) -> MarketDataResult<f64>;
    fn submit_order(&self, req: &OrderRequest) -> MarketDataResult<String>;
    fn cancel_order(&self, order_id: &str) -> MarketDataResult<()>;
}

struct SdkLongportClient {
    quote: QuoteContextSync,
    trade: TradeContextSync,
    portfolio: PortfolioContextSync,
    account_currency: Option<String>,
    trade_rate_limit: TradeRateLimit,
}

impl SdkLongportClient {
    fn new(config: Arc<Config>, account_currency: Option<String>) -> Result<Self, String> {
        Ok(Self {
            quote: QuoteContextSync::new(Arc::clone(&config), |_| {}),
            trade: TradeContextSync::new(Arc::clone(&config), |_| {}),
            portfolio: PortfolioContextSync::new(config)
                .map_err(longport_error("portfolio context"))?,
            account_currency: account_currency
                .map(|value| value.trim().to_uppercase())
                .filter(|value| !value.is_empty()),
            trade_rate_limit: TradeRateLimit::default(),
        })
    }

    fn sdk_quote(&self, symbol: &str) -> MarketDataResult<Quote> {
        let symbol = normalize_symbol(symbol);
        let quote = self
            .quote
            .quote([symbol.clone()])
            .map_err(longport_error("quote"))?
            .into_iter()
            .next()
            .ok_or_else(|| format!("LongPort returned no quote for {symbol}"))?;
        let depth = self
            .quote
            .depth(symbol.clone())
            .map_err(longport_error("depth"))?;
        let last = decimal_to_f64(quote.last_done, "last_done")?;
        let bid = first_depth_price(&depth.bids).unwrap_or(last);
        let ask = first_depth_price(&depth.asks).unwrap_or(last);
        let price = if last > 0.0 {
            last
        } else if bid > 0.0 && ask > 0.0 {
            (bid + ask) / 2.0
        } else {
            return Err(format!("LongPort returned no positive price for {symbol}"));
        };
        let bid = if bid > 0.0 { bid } else { price };
        let ask = if ask > 0.0 { ask } else { price };
        if bid > ask {
            return Err(format!("LongPort returned crossed depth for {symbol}"));
        }
        Ok(Quote {
            symbol: quote.symbol,
            price,
            bid,
            ask,
            timestamp_ms: unix_nanos_to_millis(quote.timestamp.unix_timestamp_nanos()),
            source: SOURCE.to_string(),
        })
    }

    fn sdk_portfolio(&self, symbol: Option<&str>) -> MarketDataResult<PortfolioSnapshot> {
        self.trade_rate_limit.wait();
        let balances = self
            .trade
            .account_balance(self.account_currency.as_deref())
            .map_err(longport_error("account balance"))?;
        let account = select_account(&balances, self.account_currency.as_deref())?;
        let currency = account.currency.clone();

        let position_options = symbol
            .map(str::trim)
            .filter(|symbol| !symbol.is_empty())
            .map(|symbol| GetStockPositionsOptions::new().symbols([normalize_symbol(symbol)]));
        self.trade_rate_limit.wait();
        let position_response = self
            .trade
            .stock_positions(position_options)
            .map_err(longport_error("stock positions"))?;

        self.trade_rate_limit.wait();
        let raw_orders = self
            .trade
            .today_orders(None::<GetTodayOrdersOptions>)
            .map_err(longport_error("today orders"))?;

        let raw_positions = position_response
            .channels
            .into_iter()
            .flat_map(|channel| channel.positions)
            .collect::<Vec<_>>();
        let needs_exchange_rates = raw_positions
            .iter()
            .any(|position| !position.currency.eq_ignore_ascii_case(&currency))
            || raw_orders
                .iter()
                .any(|order| !order.currency.eq_ignore_ascii_case(&currency));
        let exchange_rates = if needs_exchange_rates {
            self.portfolio
                .exchange_rate()
                .map_err(longport_error("exchange rates"))?
        } else {
            ExchangeRates { exchanges: vec![] }
        };
        let symbols = raw_positions
            .iter()
            .map(|position| position.symbol.clone())
            .collect::<Vec<_>>();
        let quote_by_symbol = if symbols.is_empty() {
            BTreeMap::new()
        } else {
            self.quote
                .quote(symbols)
                .map_err(longport_error("position quotes"))?
                .into_iter()
                .map(|quote| {
                    let price = decimal_to_f64(quote.last_done, "position last_done")?;
                    Ok((price.is_finite() && price > 0.0).then_some((quote.symbol, price)))
                })
                .collect::<MarketDataResult<Vec<_>>>()?
                .into_iter()
                .flatten()
                .collect()
        };

        let positions = raw_positions
            .into_iter()
            .map(|position| {
                let quantity = decimal_to_f64(position.quantity, "position quantity")?;
                let factor = exchange_factor(&exchange_rates, &position.currency, &currency)?;
                let average_price =
                    decimal_to_f64(position.cost_price, "position cost_price")? * factor;
                let market_price = quote_by_symbol
                    .get(&position.symbol)
                    .copied()
                    .map(|price| price * factor)
                    .unwrap_or(average_price);
                let market_value = market_price * quantity;
                Ok(Position {
                    symbol: position.symbol,
                    quantity,
                    average_price,
                    market_price,
                    market_value,
                    unrealized_pnl: (market_price - average_price) * quantity,
                })
            })
            .collect::<MarketDataResult<Vec<_>>>()?;

        let requested_symbol = symbol
            .map(str::trim)
            .filter(|symbol| !symbol.is_empty())
            .map(normalize_symbol);
        let open_orders = raw_orders
            .into_iter()
            .filter(|order| is_open_status(order.status))
            .filter(|order| {
                requested_symbol
                    .as_ref()
                    .is_none_or(|symbol| order.symbol.eq_ignore_ascii_case(symbol))
            })
            .map(|order| {
                let factor = exchange_factor(&exchange_rates, &order.currency, &currency)?;
                map_longport_order(order, factor)
            })
            .collect::<MarketDataResult<Vec<_>>>()?;

        let cash_info = account
            .cash_infos
            .iter()
            .find(|cash| cash.currency.eq_ignore_ascii_case(&currency));
        let cash = decimal_to_f64(account.total_cash, "total_cash")?;
        let equity = decimal_to_f64(account.net_assets, "net_assets")?;
        let buying_power = decimal_to_f64(account.buy_power, "buy_power")?;
        let gross_exposure = positions
            .iter()
            .map(|position| position.market_value.abs())
            .sum::<f64>();
        let reserved_buy_notional = open_orders
            .iter()
            .filter(|order| order.side == OrderSide::Buy)
            .map(order_notional)
            .sum::<f64>();
        let reserved_sell_notional = open_orders
            .iter()
            .filter(|order| order.side == OrderSide::Sell)
            .map(order_notional)
            .sum::<f64>();
        let available_cash = cash_info
            .map(|cash| decimal_to_f64(cash.available_cash, "available_cash"))
            .transpose()?
            .unwrap_or((cash - reserved_buy_notional).max(0.0));

        Ok(PortfolioSnapshot {
            positions,
            open_orders,
            account_mode: "longport-live".to_string(),
            account: AccountSummary {
                currency,
                cash,
                available_cash,
                equity,
                realized_pnl: 0.0,
                total_commission: 0.0,
                net_pnl: 0.0,
                gross_exposure,
                projected_gross_exposure: gross_exposure
                    + reserved_buy_notional
                    + reserved_sell_notional,
                reserved_buy_notional,
                reserved_buy_commission: 0.0,
                reserved_sell_notional,
                buying_power,
            },
            risk_limits: RiskLimits::default(),
            order_history: Vec::new(),
        })
    }
}

impl LongportClient for SdkLongportClient {
    fn quote(&self, symbol: &str) -> MarketDataResult<Quote> {
        self.sdk_quote(symbol)
    }

    fn candles(&self, symbol: &str, interval: &str, limit: usize) -> MarketDataResult<Vec<Candle>> {
        let symbol = normalize_symbol(symbol);
        let period = parse_period(interval)?;
        self.quote
            .candlesticks(
                symbol.clone(),
                period,
                limit.clamp(1, 1_000),
                AdjustType::NoAdjust,
                TradeSessions::Intraday,
            )
            .map_err(longport_error("candlesticks"))?
            .into_iter()
            .map(|candle| {
                Ok(Candle {
                    symbol: symbol.clone(),
                    interval: interval.to_string(),
                    open: decimal_to_f64(candle.open, "candle open")?,
                    high: decimal_to_f64(candle.high, "candle high")?,
                    low: decimal_to_f64(candle.low, "candle low")?,
                    close: decimal_to_f64(candle.close, "candle close")?,
                    volume: candle.volume as f64,
                    index: unix_nanos_to_millis(candle.timestamp.unix_timestamp_nanos()) as u64,
                    source: SOURCE.to_string(),
                })
            })
            .collect()
    }

    fn depth(&self, symbol: &str, limit: usize) -> MarketDataResult<OrderBook> {
        let symbol = normalize_symbol(symbol);
        let depth = self
            .quote
            .depth(symbol.clone())
            .map_err(longport_error("depth"))?;
        let limit = limit.clamp(1, 50);
        Ok(OrderBook {
            symbol,
            bids: map_depth(depth.bids, limit)?,
            asks: map_depth(depth.asks, limit)?,
            timestamp_ms: now_ms(),
            source: SOURCE.to_string(),
        })
    }

    fn portfolio(&self, symbol: Option<&str>) -> MarketDataResult<PortfolioSnapshot> {
        self.sdk_portfolio(symbol)
    }

    fn account_value(&self, symbol: &str, value: f64) -> MarketDataResult<f64> {
        let Some(account_currency) = self.account_currency.as_deref() else {
            return Ok(value);
        };
        let symbol = normalize_symbol(symbol);
        let security = self
            .quote
            .static_info([symbol.clone()])
            .map_err(longport_error("security static info"))?
            .into_iter()
            .next()
            .ok_or_else(|| format!("LongPort returned no static information for {symbol}"))?;
        if security.currency.eq_ignore_ascii_case(account_currency) {
            return Ok(value);
        }
        let rates = self
            .portfolio
            .exchange_rate()
            .map_err(longport_error("exchange rates"))?;
        Ok(value * exchange_factor(&rates, &security.currency, account_currency)?)
    }

    fn submit_order(&self, req: &OrderRequest) -> MarketDataResult<String> {
        let quantity = decimal_from_f64(req.quantity, "quantity")?;
        let order_type = match req.order_type {
            OrderType::Market => LongportOrderType::MO,
            OrderType::Limit => LongportOrderType::LO,
        };
        let side = match req.side {
            OrderSide::Buy => LongportOrderSide::Buy,
            OrderSide::Sell => LongportOrderSide::Sell,
        };
        let mut options = SubmitOrderOptions::new(
            normalize_symbol(&req.symbol),
            order_type,
            side,
            quantity,
            TimeInForceType::Day,
        )
        .remark("neenee-quant");
        if req.order_type == OrderType::Limit {
            let price = req
                .price
                .ok_or_else(|| "limit order requires a price".to_string())?;
            options = options.submitted_price(decimal_from_f64(price, "price")?);
        }
        self.trade_rate_limit.wait();
        self.trade
            .submit_order(options)
            .map(|response| response.order_id)
            .map_err(longport_error("submit order"))
    }

    fn cancel_order(&self, order_id: &str) -> MarketDataResult<()> {
        self.trade_rate_limit.wait();
        self.trade
            .cancel_order(order_id.to_string())
            .map_err(longport_error("cancel order"))
    }
}

#[derive(Default)]
struct TradeRateLimit {
    requests: Mutex<VecDeque<Instant>>,
}

impl TradeRateLimit {
    fn wait(&self) {
        loop {
            let mut requests = self.requests.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            while requests
                .front()
                .is_some_and(|first| now.duration_since(*first) >= TRADE_WINDOW)
            {
                requests.pop_front();
            }
            let interval_wait = requests
                .back()
                .map(|last| TRADE_MIN_INTERVAL.saturating_sub(now.duration_since(*last)))
                .unwrap_or_default();
            let window_wait = if requests.len() >= TRADE_WINDOW_REQUESTS {
                requests
                    .front()
                    .map(|first| TRADE_WINDOW.saturating_sub(now.duration_since(*first)))
                    .unwrap_or_default()
            } else {
                Duration::ZERO
            };
            let wait = interval_wait.max(window_wait);
            if wait.is_zero() {
                requests.push_back(now);
                return;
            }
            drop(requests);
            thread::sleep(wait);
        }
    }
}

fn parse_period(interval: &str) -> MarketDataResult<Period> {
    match interval.trim().to_ascii_lowercase().as_str() {
        "1m" => Ok(Period::OneMinute),
        "2m" => Ok(Period::TwoMinute),
        "3m" => Ok(Period::ThreeMinute),
        "5m" => Ok(Period::FiveMinute),
        "10m" => Ok(Period::TenMinute),
        "15m" => Ok(Period::FifteenMinute),
        "20m" => Ok(Period::TwentyMinute),
        "30m" => Ok(Period::ThirtyMinute),
        "45m" => Ok(Period::FortyFiveMinute),
        "60m" | "1h" => Ok(Period::SixtyMinute),
        "2h" => Ok(Period::TwoHour),
        "3h" => Ok(Period::ThreeHour),
        "4h" => Ok(Period::FourHour),
        "1d" | "day" => Ok(Period::Day),
        "1w" | "week" => Ok(Period::Week),
        "1mo" | "month" => Ok(Period::Month),
        "1q" | "quarter" => Ok(Period::Quarter),
        "1y" | "year" => Ok(Period::Year),
        other => Err(format!(
            "unsupported LongPort candle interval '{other}'; use 1m, 2m, 3m, 5m, 10m, 15m, 20m, 30m, 45m, 1h, 2h, 3h, 4h, 1d, 1w, 1mo, 1q, or 1y"
        )),
    }
}

fn first_depth_price(levels: &[longport::quote::Depth]) -> Option<f64> {
    levels
        .iter()
        .find_map(|level| {
            level
                .price
                .and_then(|price| decimal_to_f64(price, "depth").ok())
        })
        .filter(|price| *price > 0.0)
}

fn map_depth(
    levels: Vec<longport::quote::Depth>,
    limit: usize,
) -> MarketDataResult<Vec<BookLevel>> {
    levels
        .into_iter()
        .filter_map(|level| level.price.map(|price| (price, level.volume)))
        .take(limit)
        .map(|(price, volume)| {
            Ok(BookLevel {
                price: decimal_to_f64(price, "depth price")?,
                quantity: volume as f64,
            })
        })
        .collect()
}

fn select_account<'a>(
    accounts: &'a [longport::trade::AccountBalance],
    preferred_currency: Option<&str>,
) -> MarketDataResult<&'a longport::trade::AccountBalance> {
    preferred_currency
        .and_then(|currency| {
            accounts
                .iter()
                .find(|account| account.currency.eq_ignore_ascii_case(currency))
        })
        .or_else(|| accounts.first())
        .ok_or_else(|| "LongPort returned no account balance".to_string())
}

fn map_longport_order(
    order: longport::trade::Order,
    currency_factor: f64,
) -> MarketDataResult<Order> {
    let order_type = match order.order_type {
        LongportOrderType::MO | LongportOrderType::AO | LongportOrderType::MIT => OrderType::Market,
        _ => OrderType::Limit,
    };
    let side = match order.side {
        LongportOrderSide::Sell => OrderSide::Sell,
        _ => OrderSide::Buy,
    };
    Ok(Order {
        order_id: order.order_id,
        status: order.status.to_string(),
        symbol: order.symbol,
        side,
        order_type,
        quantity: decimal_to_f64(order.quantity, "order quantity")?,
        limit_price: order
            .price
            .map(|price| decimal_to_f64(price, "order price").map(|price| price * currency_factor))
            .transpose()?,
        fill_price: order
            .executed_price
            .map(|price| {
                decimal_to_f64(price, "executed price").map(|price| price * currency_factor)
            })
            .transpose()?,
        filled_quantity: decimal_to_f64(order.executed_quantity, "executed quantity")?,
        commission: 0.0,
        timestamp_ms: unix_nanos_to_millis(order.submitted_at.unix_timestamp_nanos()),
    })
}

fn exchange_factor(rates: &ExchangeRates, from: &str, to: &str) -> MarketDataResult<f64> {
    if from.eq_ignore_ascii_case(to) {
        return Ok(1.0);
    }
    if let Some(rate) = rates.exchanges.iter().find(|rate| {
        rate.base_currency.eq_ignore_ascii_case(from)
            && rate.other_currency.eq_ignore_ascii_case(to)
    }) {
        return positive_rate(rate.average_rate, from, to);
    }
    if let Some(rate) = rates.exchanges.iter().find(|rate| {
        rate.base_currency.eq_ignore_ascii_case(to)
            && rate.other_currency.eq_ignore_ascii_case(from)
    }) {
        return positive_rate(rate.average_rate, from, to).map(|rate| 1.0 / rate);
    }
    Err(format!(
        "LongPort returned no exchange rate from {from} to {to}"
    ))
}

fn positive_rate(rate: f64, from: &str, to: &str) -> MarketDataResult<f64> {
    if rate.is_finite() && rate > 0.0 {
        Ok(rate)
    } else {
        Err(format!(
            "LongPort returned an invalid exchange rate from {from} to {to}"
        ))
    }
}

fn is_open_status(status: LongportOrderStatus) -> bool {
    matches!(
        status,
        LongportOrderStatus::NotReported
            | LongportOrderStatus::ReplacedNotReported
            | LongportOrderStatus::ProtectedNotReported
            | LongportOrderStatus::VarietiesNotReported
            | LongportOrderStatus::WaitToNew
            | LongportOrderStatus::New
            | LongportOrderStatus::WaitToReplace
            | LongportOrderStatus::PendingReplace
            | LongportOrderStatus::PartialFilled
            | LongportOrderStatus::WaitToCancel
            | LongportOrderStatus::PendingCancel
    )
}

fn order_notional(order: &Order) -> f64 {
    order.limit_price.or(order.fill_price).unwrap_or_default()
        * (order.quantity - order.filled_quantity).max(0.0)
}

fn order_risk_price(req: &OrderRequest, quote: &Quote) -> MarketDataResult<f64> {
    match req.order_type {
        OrderType::Market => {
            let price = match req.side {
                OrderSide::Buy => quote.ask,
                OrderSide::Sell => quote.bid,
            };
            if price.is_finite() && price > 0.0 {
                Ok(price)
            } else {
                Err("market order requires a positive finite broker-side quote".to_string())
            }
        }
        OrderType::Limit => req
            .price
            .filter(|price| price.is_finite() && *price > 0.0)
            .ok_or_else(|| "limit order requires a positive finite price".to_string()),
    }
}

fn decimal_to_f64(value: Decimal, field: &str) -> MarketDataResult<f64> {
    value
        .to_string()
        .parse::<f64>()
        .map_err(|e| format!("LongPort {field} is not representable as f64: {e}"))
}

fn decimal_from_f64(value: f64, field: &str) -> MarketDataResult<Decimal> {
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{field} must be a positive finite number"));
    }
    Decimal::from_str(&value.to_string())
        .map_err(|e| format!("{field} cannot be represented as a decimal: {e}"))
}

fn longport_error(operation: &'static str) -> impl FnOnce(longport::Error) -> String {
    move |err| format!("LongPort {operation} failed: {err}")
}

fn normalize_symbol(symbol: &str) -> String {
    symbol.trim().to_uppercase()
}

fn unix_nanos_to_millis(nanos: i128) -> u128 {
    nanos.max(0) as u128 / 1_000_000
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DefaultRiskPolicy, NoopAuditSink};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeClient {
        submits: AtomicUsize,
        cancels: AtomicUsize,
        portfolio_available: std::sync::atomic::AtomicBool,
    }

    impl FakeClient {
        fn new() -> Self {
            Self {
                submits: AtomicUsize::new(0),
                cancels: AtomicUsize::new(0),
                portfolio_available: std::sync::atomic::AtomicBool::new(true),
            }
        }

        fn account() -> AccountSummary {
            AccountSummary {
                currency: "USD".to_string(),
                cash: 10_000.0,
                available_cash: 10_000.0,
                equity: 10_000.0,
                realized_pnl: 0.0,
                total_commission: 0.0,
                net_pnl: 0.0,
                gross_exposure: 0.0,
                projected_gross_exposure: 0.0,
                reserved_buy_notional: 0.0,
                reserved_buy_commission: 0.0,
                reserved_sell_notional: 0.0,
                buying_power: 10_000.0,
            }
        }
    }

    impl LongportClient for FakeClient {
        fn quote(&self, symbol: &str) -> MarketDataResult<Quote> {
            Ok(Quote {
                symbol: normalize_symbol(symbol),
                price: 100.0,
                bid: 99.0,
                ask: 101.0,
                timestamp_ms: 1,
                source: SOURCE.to_string(),
            })
        }

        fn candles(
            &self,
            symbol: &str,
            interval: &str,
            _limit: usize,
        ) -> MarketDataResult<Vec<Candle>> {
            Ok(vec![Candle {
                symbol: normalize_symbol(symbol),
                interval: interval.to_string(),
                open: 99.0,
                high: 102.0,
                low: 98.0,
                close: 100.0,
                volume: 10.0,
                index: 1,
                source: SOURCE.to_string(),
            }])
        }

        fn depth(&self, symbol: &str, _limit: usize) -> MarketDataResult<OrderBook> {
            Ok(OrderBook {
                symbol: normalize_symbol(symbol),
                bids: vec![BookLevel {
                    price: 99.0,
                    quantity: 1.0,
                }],
                asks: vec![BookLevel {
                    price: 101.0,
                    quantity: 1.0,
                }],
                timestamp_ms: 1,
                source: SOURCE.to_string(),
            })
        }

        fn portfolio(&self, _symbol: Option<&str>) -> MarketDataResult<PortfolioSnapshot> {
            if !self.portfolio_available.load(Ordering::Relaxed) {
                return Err("portfolio unavailable".to_string());
            }
            Ok(PortfolioSnapshot {
                positions: Vec::new(),
                open_orders: Vec::new(),
                account_mode: "longport-live".to_string(),
                account: Self::account(),
                risk_limits: RiskLimits::default(),
                order_history: Vec::new(),
            })
        }

        fn account_value(&self, _symbol: &str, value: f64) -> MarketDataResult<f64> {
            Ok(value)
        }

        fn submit_order(&self, _req: &OrderRequest) -> MarketDataResult<String> {
            self.submits.fetch_add(1, Ordering::Relaxed);
            Ok("LONGPORT-ORDER-1".to_string())
        }

        fn cancel_order(&self, _order_id: &str) -> MarketDataResult<()> {
            self.cancels.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    fn adapter(client: Arc<FakeClient>, limits: RiskLimits) -> LongportAdapter {
        LongportAdapter::with_client(
            client,
            Arc::new(DefaultRiskPolicy::new(limits)),
            Arc::new(NoopAuditSink::default()),
        )
    }

    #[test]
    fn supported_intervals_map_to_sdk_periods() {
        assert_eq!(parse_period("1m").unwrap(), Period::OneMinute);
        assert_eq!(parse_period("1h").unwrap(), Period::SixtyMinute);
        assert_eq!(parse_period("1d").unwrap(), Period::Day);
        assert_eq!(parse_period("1mo").unwrap(), Period::Month);
        assert!(parse_period("7h").unwrap_err().contains("unsupported"));
    }

    #[test]
    fn quote_and_depth_delegate_to_longport_client() {
        let client = Arc::new(FakeClient::new());
        let adapter = adapter(client, RiskLimits::default());

        assert_eq!(adapter.quote("aapl.us").unwrap().symbol, "AAPL.US");
        assert_eq!(adapter.depth("aapl.us", 10).unwrap().bids.len(), 1);
    }

    #[test]
    fn local_risk_rejection_never_submits_to_longport() {
        let client = Arc::new(FakeClient::new());
        let adapter = adapter(
            Arc::clone(&client),
            RiskLimits {
                max_order_notional: 50.0,
                ..RiskLimits::default()
            },
        );
        let decision = adapter.place_order(
            OrderRequest {
                symbol: "AAPL.US".to_string(),
                side: OrderSide::Buy,
                order_type: OrderType::Market,
                quantity: 1.0,
                price: None,
            },
            client.quote("AAPL.US").unwrap(),
        );

        assert_eq!(decision.status, "rejected_risk");
        assert_eq!(client.submits.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn accepted_order_returns_longport_order_id() {
        let client = Arc::new(FakeClient::new());
        let adapter = adapter(Arc::clone(&client), RiskLimits::default());
        let decision = adapter.place_order(
            OrderRequest {
                symbol: "AAPL.US".to_string(),
                side: OrderSide::Buy,
                order_type: OrderType::Limit,
                quantity: 1.0,
                price: Some(100.0),
            },
            client.quote("AAPL.US").unwrap(),
        );

        assert_eq!(decision.status, "submitted_longport");
        assert_eq!(
            decision.order.as_ref().map(|order| order.order_id.as_str()),
            Some("LONGPORT-ORDER-1")
        );
        assert_eq!(client.submits.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cancellation_is_not_blocked_by_a_portfolio_refresh_failure() {
        let client = Arc::new(FakeClient::new());
        client.portfolio_available.store(false, Ordering::Relaxed);
        let adapter = adapter(Arc::clone(&client), RiskLimits::default());

        let decision = adapter.cancel_order(CancelOrderRequest {
            order_id: "LONGPORT-ORDER-1".to_string(),
        });

        assert_eq!(decision.status, "cancelled_longport");
        assert_eq!(client.cancels.load(Ordering::Relaxed), 1);
        assert_eq!(decision.account.currency, "longport");
    }

    #[test]
    fn exchange_rates_convert_both_directions() {
        let rates = ExchangeRates {
            exchanges: vec![longport::portfolio::ExchangeRate {
                average_rate: 7.79,
                base_currency: "USD".to_string(),
                bid_rate: 7.78,
                offer_rate: 7.80,
                other_currency: "HKD".to_string(),
            }],
        };

        assert!((exchange_factor(&rates, "HKD", "USD").unwrap() - 1.0 / 7.79).abs() < 1e-12);
        assert_eq!(exchange_factor(&rates, "USD", "HKD").unwrap(), 7.79);
        assert_eq!(exchange_factor(&rates, "USD", "USD").unwrap(), 1.0);
        assert!(exchange_factor(&rates, "EUR", "USD").is_err());
    }
}

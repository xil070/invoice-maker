use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Address {
    pub street: String,
    pub city: String,
    pub state: String,
    pub zip: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Project {
    pub id: String,
    pub name: Option<String>,
    pub address: Address,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClientConfig {
    pub name: String,         // 公司名 或 人名
    pub attn: Option<String>, // 新增：联系人
    pub email: Option<String>,
    pub billing_address: Option<Address>,
    #[serde(default)] 
    pub projects: Vec<Project>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct InvoiceItem {
    pub description: String,
    pub quantity: f64,
    pub rate: f64,
    pub amount: f64, 
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SenderConfig {
    pub name: String,
    pub address1: String,
    pub address2: String,
    pub license: String,
    pub email: String,
    pub phone: String,
    pub bank_info: String,
}

#[derive(Serialize)]
pub struct InvoiceContext {
    pub id: String,
    pub date: String,
    pub sender: SenderConfig,
    pub client: ClientConfig,
    pub project: Project,
    pub items: Vec<InvoiceItem>,
    pub total: f64,
    pub tax_rate: f64,
    pub is_paid: bool,
    pub is_void: bool,
    pub tax_display: String,
}
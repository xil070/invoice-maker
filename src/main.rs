mod model;

use clap::{Parser, Subcommand};
use comfy_table::{Cell, Table, Attribute, Color};
use inquire::{Confirm, DateSelect, Select, Text};
use regex::Regex;
use serde::{Deserialize, Serialize};
use slug::slugify;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tera::{Context, Tera};
use zipcodes;
use chrono::{Datelike, Local, NaiveDate};
use directories::{BaseDirs, ProjectDirs};

use crate::model::{ClientConfig, Address, Project, InvoiceItem, InvoiceContext, SenderConfig};

// ==========================================
// Constants & Embeds
// ==========================================
const NEW_CLIENT_OPT: &str = "➕ Add New Client";
const NEW_PROJECT_OPT: &str = "➕ Add New Project";

// Embed template at compile time to ensure availability
const DEFAULT_TEMPLATE: &str = include_str!("../templates/invoice.tera");

// ==========================================
// Structs & Enums
// ==========================================

#[derive(Debug, Serialize, Deserialize)]
struct AppSettings {
    data_root: String,
}

#[derive(Parser)]
#[command(name = "invoice-maker")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new invoice
    New,
    /// Add a new client
    AddClient,
    /// Configure data directory
    Config,
    /// Mark invoice as PAID (hides already paid)
    Pay,
    /// Revert invoice to UNPAID (hides unpaid)
    Unpay,
    /// List all PAID invoices
    Paid,
    /// List all UNPAID invoices
    Unpaid,
    /// Open output folder
    Open,
    /// Show summary of invoices
    Summary {
        /// Year to summarize (defaults to current year)
        year: Option<i32>,
    },
    /// Void an invoice
    Void,
}

// ==========================================
// Main Function
// ==========================================

fn main() {
    let cli = Cli::parse();
    
    // 1. Initialize configuration
    let settings = load_settings().unwrap_or_else(|| setup_config_wizard());
    let expanded_path = expand_home_dir(&settings.data_root);
    let root = PathBuf::from(expanded_path);
    let data_dir = root.join("data/clients");
    
    // Ensure data directory exists
    if let Err(e) = fs::create_dir_all(&data_dir) {
        eprintln!("❌ Error: Failed to create data directory: {}", e);
        return;
    }

    // Load sender configuration
    let sender_config = load_sender_config(&root);

    if cli.command.is_none() {
        use clap::CommandFactory;
        Cli::command().print_help().unwrap();
        return;
    }

    match cli.command.unwrap() {
        Commands::New => {
            let client_id = select_or_create_client(&data_dir);
            println!("✅ Selected Client: {}", client_id);

            let (client_config, selected_project) = select_or_create_project(&data_dir, &client_id);
            println!("✅ Selected Project: {} ({})", selected_project.name.as_deref().unwrap_or("No Name"), selected_project.address.street);

            let items = enter_invoice_items();
            
            if !items.is_empty() {
                // Date selection
                let date = DateSelect::new("Invoice Date:")
                    .with_default(Local::now().date_naive())
                    .prompt()
                    .unwrap();

                let (tax_rate, tax_status) = ask_for_tax();
                
                generate_pdf(&root, &client_id, &client_config, &selected_project, &items, tax_rate, date, tax_status, &sender_config);
            } else {
                println!("❌ No items entered. Aborting.");
            }
        }
        Commands::AddClient => {
            create_client_wizard(&data_dir);
        }
        Commands::Config => {
            setup_config_wizard();
        }
        Commands::Pay => {
            // true = Mark as Paid (show only unpaid)
            change_invoice_status(&root, true);
        }
        Commands::Unpay => {
            // false = Mark as Unpaid (show only paid)
            change_invoice_status(&root, false);
        }
        Commands::Paid => {
            list_invoices_by_status(&root, true);
        }
        Commands::Unpaid => {
            list_invoices_by_status(&root, false);
        }
        Commands::Open => {
            open_folder_wizard(&root);
        }
        Commands::Summary { year } => {
            show_summary(&root, year);
        }
        Commands::Void => {
            void_invoice(&root);
        }
    }
}

// ==========================================
// 1. Client & Project Logic
// ==========================================

fn select_or_create_client(data_dir: &Path) -> String {
    let mut options = vec![NEW_CLIENT_OPT.to_string()];
    
    if let Ok(entries) = fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Ok(name) = entry.file_name().into_string() {
                    options.push(name);
                }
            }
        }
    }

    let ans = Select::new("Please Select Client (Type to Filter):", options).prompt();

    match ans {
        Ok(choice) => {
            if choice == NEW_CLIENT_OPT {
                create_client_wizard(data_dir)
            } else {
                choice
            }
        },
        Err(_) => std::process::exit(0),
    }
}

// Create Client Wizard
fn create_client_wizard(data_dir: &Path) -> String {
    println!("\n--- Creating New Client ---");

    // 1. Ask for Company Name (Optional)
    let company_input = Text::new("Company Name (Optional, press Enter to skip):").prompt().unwrap();
    let company = if company_input.trim().is_empty() { None } else { Some(company_input.trim().to_string()) };

    // 2. Adjust contact person prompt based on company presence
    let attn_prompt = if company.is_some() { "Attn / Contact Person:" } else { "Client Name:" };
    let attn_input = Text::new(attn_prompt).prompt().unwrap();
    
    // 3. Determine ID (prefer company slug, fallback to person slug)
    let raw_name_for_id = if let Some(c) = &company { c } else { &attn_input };
    let id = slugify(raw_name_for_id);

    // 4. Determine fields for ClientConfig
    // If company exists: Name = Company, Attn = Person
    // If no company: Name = Person, Attn = None
    let (final_name, final_attn) = if let Some(c) = company {
        (c, Some(attn_input))
    } else {
        (format!("Attn: {}", attn_input), None)
    };

    let email_input = Text::new("Client Email (Optional):").prompt().unwrap();
    let email = if email_input.trim().is_empty() { None } else { Some(email_input) };

    println!("\n--- Enter Client Billing Address (Optional) ---");
    let billing_address = wizard_address_new_order(true);

    let client = ClientConfig {
        name: final_name,
        attn: final_attn,
        email,
        billing_address,
        projects: vec![],
    };

    let client_path = data_dir.join(&id);
    if client_path.exists() {
        println!("⚠️  Client ID {} already exists, using existing folder.", id);
    } else {
        fs::create_dir_all(&client_path).expect("Creating client directory failed");
    }
    
    let toml_str = toml::to_string_pretty(&client).unwrap();
    fs::write(client_path.join("info.toml"), toml_str).expect("Failed to write info.toml");

    println!("✅ Client created successfully: {}", id);
    id
}

fn select_or_create_project(data_dir: &Path, client_id: &str) -> (ClientConfig, Project) {
    let config_path = data_dir.join(client_id).join("info.toml");
    let content = fs::read_to_string(&config_path).expect("Failed to read client config");
    let mut config: ClientConfig = toml::from_str(&content).expect("TOML parsing failed");

    let mut options = Vec::new();
    options.push(NEW_PROJECT_OPT.to_string());
    
    for p in &config.projects {
        let display_name = p.name.as_deref().unwrap_or("Project");
        options.push(format!("{} | {}", display_name, p.address.street));
    }

    let ans = Select::new("Select Project / Job Site:", options).prompt().unwrap();

    if ans == NEW_PROJECT_OPT {
        println!("\n--- Adding New Project ---");
        
        let name_input = Text::new("Project Name (Optional):").prompt().unwrap();
        let name = if name_input.trim().is_empty() { None } else { Some(name_input) };
        
        println!("--- Enter Project Address ---");
        
        let address;
        let mut reused_billing = false;
        
        if let Some(billing) = &config.billing_address {
            println!("Found Billing Address: {}, {}, {}", billing.street, billing.city, billing.state);
            let same = Confirm::new("Use same address as billing?")
                .with_default(true)
                .prompt()
                .unwrap();
            
            if same {
                address = billing.clone();
                reused_billing = true;
            } else {
                address = Address { street: "".into(), city: "".into(), state: "".into(), zip: "".into() };
            }
        } else {
             address = Address { street: "".into(), city: "".into(), state: "".into(), zip: "".into() };
        }

        let final_address = if reused_billing {
            address
        } else {
            wizard_address_new_order(false).expect("Project address is required!")
        };

        let id = slugify(&final_address.street);

        let new_project = Project {
            id,
            name,
            address: final_address,
        };

        config.projects.push(new_project.clone());
        let new_toml = toml::to_string_pretty(&config).unwrap();
        fs::write(config_path, new_toml).expect("Failed to update info.toml");

        println!("✅ Project added to database!");
        (config, new_project)
    } else {
        let selected_street = ans.split(" | ").last().unwrap();
        let project = config.projects.iter().find(|p| p.address.street == selected_street).unwrap().clone();
        (config, project)
    }
}

// ==========================================
// 2. Data Entry Helpers
// ==========================================

fn wizard_address_new_order(is_optional: bool) -> Option<Address> {
    let street_prompt = if is_optional { "Street (Leave empty to skip):" } else { "Street (Required):" };
    let street = Text::new(street_prompt).prompt().unwrap();

    if is_optional && street.trim().is_empty() {
        return None;
    }

    let zip = Text::new("Zip Code (Leave empty to skip lookup):").prompt().unwrap();
    let (mut def_city, mut def_state) = (String::new(), String::new());

    if !zip.trim().is_empty() {
        match zipcodes::matching(&zip, None) {
            Ok(results) => {
                if let Some(info) = results.first() {
                    println!("🚀 Found: {}, {}", info.city, info.state);
                    def_city = info.city.to_string();
                    def_state = info.state.to_string();
                }
            },
            Err(_) => {}
        }
    }

    let city = Text::new("City:").with_default(&def_city).prompt().unwrap();
    let state = Text::new("State:").with_default(&def_state).prompt().unwrap();

    Some(Address { street, city, state, zip })
}

// Returns (tax_rate, status_text)
fn ask_for_tax() -> (f64, String) {
    let apply_tax = Confirm::new("Add Tax to Total?").with_default(true).prompt().unwrap();
    
    if apply_tax {
        let rate_str = Text::new("Tax Rate % (e.g. 8.875):").with_default("8.875").prompt().unwrap();
        let rate: f64 = rate_str.parse().unwrap_or(0.0);
        // If adding tax, return rate. Status text is generated later.
        (rate / 100.0, "ADD".to_string()) 
    } else {
        // If not adding tax, ask for reason
        let options = vec!["Exempt", "Included"];
        let status = Select::new("Tax Status:", options).prompt().unwrap();
        (0.0, status.to_string())
    }
}

fn enter_invoice_items() -> Vec<InvoiceItem> {
    let mut items = Vec::new();
    println!("\n--- Enter Invoice Items ---");
    println!("💡 Tip: Use '\\n' for new lines, and '- ' for bullet points."); 
    println!("(Leave Description empty to finish)");

    loop {
        let desc = Text::new("Description (leave empty to finish):").prompt().unwrap();
        
        if desc.trim().is_empty() {
            break;
        }

        let amount_str = Text::new("Amount ($):").prompt().unwrap();
        let amount: f64 = amount_str.parse().unwrap_or(0.0);

        items.push(InvoiceItem {
            description: desc,
            quantity: 1.0,
            rate: amount,
            amount: amount,
        });
    }
    items
}

// ==========================================
// 3. PDF Generation (New Logic)
// ==========================================

fn generate_pdf(
    root: &Path, 
    client_id: &str, 
    client: &ClientConfig, 
    project: &Project, 
    items: &[InvoiceItem],
    tax_rate: f64,
    date: NaiveDate, // Date parameter
    tax_status: String,
    sender: &SenderConfig,
) {
    // Check if Typst is installed
    if Command::new("typst").arg("--version").output().is_err() {
        println!("❌ Error: 'typst' is not installed. Please install it (brew install typst).");
        return;
    }

    // Initialize template
    let template_dir = root.join("templates");
    if !template_dir.exists() { fs::create_dir_all(&template_dir).unwrap(); }
    let template_path = template_dir.join("invoice.tera");
    if !template_path.exists() { 
        println!("✨ Initializing default template...");
        fs::write(&template_path, DEFAULT_TEMPLATE).expect("Failed to write default template");
    }

    let tera = match Tera::new(template_dir.join("*.tera").to_str().unwrap()) {
        Ok(t) => t,
        Err(e) => { println!("❌ Template Error: {}", e); return; }
    };

    // Calculate totals
    let total_before_tax: f64 = items.iter().map(|i| i.amount).sum();
    let tax_amount = total_before_tax * tax_rate;
    let total = total_before_tax + tax_amount;

    let tax_display_str = if tax_rate > 0.0 {
        format!("${:.2}", tax_amount) // Show amount if tax exists
    } else {
        tax_status // Show "Exempt" or "Included" if no tax
    };
    
    // --- Invoice ID Generation (HI20251214-01) ---
    let date_str = date.format("%Y%m%d").to_string(); // 20251214
    let prefix = format!("HI{}", date_str); // HI20251214
    
    // Scan output directory for current year to find max index
    let output_root = root.join("output");
    let mut next_idx = 1;

    let year_dir = output_root.join(date.format("%Y").to_string());
    if year_dir.exists() {
        let mut stack = vec![year_dir];
        while let Some(dir) = stack.pop() {
             if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if let Some(fname) = path.file_name() {
                        let fname_str = fname.to_string_lossy();
                        if fname_str.starts_with(&prefix) {
                            // Filename format: HI20251214-01_xxx.typ
                            // Extract part after prefix
                            let rest = &fname_str[prefix.len()..]; 
                            if rest.starts_with("-") {
                                // Parse index
                                let num_part: String = rest.chars()
                                    .skip(1) // Skip '-'
                                    .take_while(|c| c.is_numeric())
                                    .collect();
                                if let Ok(idx) = num_part.parse::<u32>() {
                                    if idx >= next_idx {
                                        next_idx = idx + 1;
                                    }
                                }
                            }
                        }
                    }
                }
             }
        }
    }

    let invoice_id = format!("{}-{:02}", prefix, next_idx); // e.g., HI20251214-01s

    // Construct Context
    let date_today = Local::now().date_naive();

    let context_data = InvoiceContext {
        id: invoice_id.clone(),
        date: date_today.format("%m/%d/%Y").to_string(),
        sender: sender.clone(),
        client: client.clone(),
        project: project.clone(),
        items: items.to_vec(),
        total,
        tax_rate,
        // Hardcoded Footer Content
        is_void: false,
        is_paid: false,
        tax_display: tax_display_str,
    };

    let context = Context::from_serialize(&context_data).unwrap();
    let rendered = tera.render("invoice.tera", &context).unwrap();

    let output_dir = output_root.join(date.format("%Y").to_string()).join(client_id);
    fs::create_dir_all(&output_dir).unwrap();

    // Filename: HI20251214-01_ProjectID.pdf
    let filename_base = format!("{}_{}", invoice_id, project.id);
    let typ_path = output_dir.join(format!("{}.typ", filename_base));
    let pdf_path = output_dir.join(format!("{}.pdf", filename_base));

    fs::write(&typ_path, rendered).expect("Failed to write .typ file");

    println!("\n🔨 Compiling PDF...");
    match Command::new("typst").arg("compile").arg(&typ_path).arg(&pdf_path).status() {
        Ok(s) if s.success() => {
            println!("✅ PDF Generated: {:?}", pdf_path);
            open_and_reveal(&pdf_path);
        },
        _ => println!("❌ Compilation failed."),
    }
}

// ==========================================
// 4. Pay / Unpay Logic (Filters & Rename)
// ==========================================

fn change_invoice_status(root: &Path, target_paid: bool) {
    let output_dir = root.join("output");
    if !output_dir.exists() { println!("❌ No output directory found."); return; }
    
    println!("🔍 Scanning invoices...");
    let mut files = Vec::new();
    let mut stack = vec![output_dir];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().map_or(false, |e| e == "typ") {
                    files.push(path);
                }
            }
        }
    }

    // Filter logic
    let filtered_files: Vec<PathBuf> = files.into_iter().filter(|p| {
        let name = p.file_stem().unwrap().to_string_lossy();
        if name.ends_with("_VOID") { return false; } // Skip voided invoices

        let is_currently_paid = name.ends_with("_PAID");
        if target_paid {
            !is_currently_paid // Pay: Select only unpaid
        } else {
            is_currently_paid  // Unpay: Select only paid
        }
    }).collect();

    if filtered_files.is_empty() {
        println!("❌ No matching invoices found.");
        return;
    }
    
    // Sort
    let mut sorted_files = filtered_files;
    sorted_files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    sorted_files.reverse();

    let options: Vec<String> = sorted_files.iter()
        .map(|p| p.strip_prefix(root.join("output")).unwrap_or(p).to_string_lossy().to_string())
        .collect();

    let action_name = if target_paid { "Mark as PAID" } else { "Mark as UNPAID" };
    
    let selection = Select::new(&format!("Select Invoice to {}:", action_name), options)
        .with_page_size(10)
        .prompt();

    match selection {
        Ok(choice) => {
            let old_typ_path = root.join("output").join(&choice);
            let old_pdf_path = old_typ_path.with_extension("pdf");

            if let Ok(content) = fs::read_to_string(&old_typ_path) {
                // Replace is_paid status
                let from_str = if target_paid { "is_paid: false" } else { "is_paid: true" };
                let to_str   = if target_paid { "is_paid: true" }  else { "is_paid: false" };
                
                let new_content = content.replace(from_str, to_str);
                
                // Calculate new filename
                let parent = old_typ_path.parent().unwrap();
                let stem = old_typ_path.file_stem().unwrap().to_string_lossy();
                
                let new_stem = if target_paid {
                    format!("{}_PAID", stem) // Add suffix
                } else {
                    stem.replace("_PAID", "") // Remove suffix
                };

                let new_typ_path = parent.join(format!("{}.typ", new_stem));
                let new_pdf_path = parent.join(format!("{}.pdf", new_stem));

                fs::write(&new_typ_path, new_content).expect("Failed to write updated .typ");
                
                // Rename and cleanup
                if new_typ_path != old_typ_path {
                    println!("♻️  Renaming to: {}", new_stem);
                    fs::remove_file(&old_typ_path).ok();
                    if old_pdf_path.exists() { fs::remove_file(&old_pdf_path).ok(); }
                }

                println!("🔨 Re-compiling...");
                match Command::new("typst").arg("compile").arg(&new_typ_path).arg(&new_pdf_path).status() {
                    Ok(s) if s.success() => {
                        println!("✅ Done!");
                        open_and_reveal(&new_pdf_path);
                    },
                    _ => println!("❌ Re-compilation failed."),
                }
            }
        },
        Err(_) => println!("Cancelled"),
    }
}

fn void_invoice(root: &Path) {
    let output_dir = root.join("output");
    if !output_dir.exists() { println!("❌ No output directory found."); return; }
    
    println!("🔍 Scanning invoices...");
    let mut files = Vec::new();
    let mut stack = vec![output_dir];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().map_or(false, |e| e == "typ") {
                    files.push(path);
                }
            }
        }
    }

    // Filter out already voided invoices and paid invoices
    let filtered_files: Vec<PathBuf> = files.into_iter().filter(|p| {
        let name = p.file_stem().unwrap().to_string_lossy();
        !name.ends_with("_VOID") && !name.ends_with("_PAID")
    }).collect();

    if filtered_files.is_empty() {
        println!("❌ No matching invoices found.");
        return;
    }
    
    // Sort
    let mut sorted_files = filtered_files;
    sorted_files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    sorted_files.reverse();

    let options: Vec<String> = sorted_files.iter()
        .map(|p| p.strip_prefix(root.join("output")).unwrap_or(p).to_string_lossy().to_string())
        .collect();

    let selection = Select::new("Select Invoice to VOID:", options)
        .with_page_size(10)
        .prompt();

    match selection {
        Ok(choice) => {
            let old_typ_path = root.join("output").join(&choice);
            let old_pdf_path = old_typ_path.with_extension("pdf");

            if let Ok(content) = fs::read_to_string(&old_typ_path) {
                // Update is_void status
                // We look for "is_void: false" and replace it with "is_void: true"
                // If "is_void" is not present (old invoices), we might need to append it, 
                // but since we updated the template and generate_pdf, new ones have it.
                // For old ones, we can just replace the end of the file or use regex.
                // But simpler: just replace "is_void: false" -> "is_void: true"
                // If it doesn't exist, we append it before the closing parenthesis.
                
                let new_content = if content.contains("is_void: false") {
                    content.replace("is_void: false", "is_void: true")
                } else {
                    // Fallback for older files: insert before the last closing parenthesis
                    // This is a bit risky if the file structure is different, but standard template ends with )
                    if let Some(last_paren) = content.rfind(')') {
                        let mut c = content.clone();
                        c.insert_str(last_paren, ", is_void: true");
                        c
                    } else {
                        content // Should not happen
                    }
                };
                
                // Calculate new filename
                let parent = old_typ_path.parent().unwrap();
                let stem = old_typ_path.file_stem().unwrap().to_string_lossy();
                let new_stem = format!("{}_VOID", stem);

                let new_typ_path = parent.join(format!("{}.typ", new_stem));
                let new_pdf_path = parent.join(format!("{}.pdf", new_stem));

                fs::write(&new_typ_path, new_content).expect("Failed to write updated .typ");
                
                // Rename/Cleanup
                if new_typ_path != old_typ_path {
                    println!("♻️  Renaming to: {}", new_stem);
                    fs::remove_file(&old_typ_path).ok();
                    if old_pdf_path.exists() { fs::remove_file(&old_pdf_path).ok(); }
                }

                println!("🔨 Re-compiling...");
                match Command::new("typst").arg("compile").arg(&new_typ_path).arg(&new_pdf_path).status() {
                    Ok(s) if s.success() => {
                        println!("✅ Done! Invoice marked as VOID.");
                        open_and_reveal(&new_pdf_path);
                    },
                    _ => println!("❌ Re-compilation failed."),
                }
            }
        },
        Err(_) => println!("Cancelled"),
    }
}

// ==========================================
// 5. List Logic
// ==========================================

fn list_invoices_by_status(root: &Path, show_paid: bool) {
    let output_dir = root.join("output");
    println!("--- List of {} Invoices ---", if show_paid { "PAID" } else { "UNPAID" });

    let mut stack = vec![output_dir];
    let mut count = 0;
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().map_or(false, |e| e == "pdf") {
                    let name = path.file_stem().unwrap().to_string_lossy();
                    if name.ends_with("_VOID") { continue; } // Skip voided

                    let is_paid = name.ends_with("_PAID");
                    
                    if is_paid == show_paid {
                        let relative = path.strip_prefix(root.join("output")).unwrap_or(&path);
                        println!("📄 {}", relative.to_string_lossy());
                        count += 1;
                    }
                }
            }
        }
    }
    if count == 0 { println!("(None found)"); }
}

// ==========================================
// 6. Open Folder Logic
// ==========================================

fn open_folder_wizard(root: &Path) {
    let output_root = root.join("output");
    let mut options = Vec::new();
    
    let root_opt = "📂 Open Root Output Directory".to_string();
    options.push(root_opt.clone());

    if output_root.exists() {
        if let Ok(years) = fs::read_dir(&output_root) {
            for year_entry in years.flatten() {
                if year_entry.path().is_dir() {
                    let year_name = year_entry.file_name().to_string_lossy().to_string();
                    if let Ok(clients) = fs::read_dir(year_entry.path()) {
                        for client_entry in clients.flatten() {
                            if client_entry.path().is_dir() {
                                let client_name = client_entry.file_name().to_string_lossy().to_string();
                                options.push(format!("{} / {}", year_name, client_name));
                            }
                        }
                    }
                }
            }
        }
    }

    let mut client_paths: Vec<String> = options.drain(1..).collect();
    client_paths.sort();
    client_paths.reverse();
    
    let mut final_options = vec![root_opt.clone()];
    final_options.extend(client_paths);

    match Select::new("Select Folder to Open:", final_options).prompt() {
        Ok(choice) => {
            let target_path = if choice == root_opt {
                output_root
            } else {
                let parts: Vec<&str> = choice.split(" / ").collect();
                if parts.len() == 2 {
                    output_root.join(parts[0]).join(parts[1])
                } else {
                    output_root
                }
            };
            println!("🚀 Opening: {:?}", target_path);
            
            #[cfg(target_os = "macos")]
            Command::new("open").arg(&target_path).spawn().ok();
            #[cfg(target_os = "windows")]
            Command::new("explorer").arg(&target_path).spawn().ok();
        },
        Err(_) => println!("Operation cancelled."),
    }
}

// ==========================================
// 7. Config & Utilities
// ==========================================

fn get_config_path() -> PathBuf {
    if let Some(proj_dirs) = ProjectDirs::from("com", "invoice-maker", "app") {
        let config_dir = proj_dirs.config_dir();
        if !config_dir.exists() { fs::create_dir_all(config_dir).ok(); }
        return config_dir.join("settings.toml");
    }
    PathBuf::from("settings.toml")
}

fn load_settings() -> Option<AppSettings> {
    let path = get_config_path();
    if !path.exists() { return None; }
    let content = fs::read_to_string(path).ok()?;
    toml::from_str(&content).ok()
}

const DEFAULT_SENDER_TEMPLATE: &str = include_str!("../sender.toml");

fn load_sender_config(root: &Path) -> SenderConfig {
    let path = root.join("sender.toml");
    if path.exists() {
        let content = fs::read_to_string(&path).expect("Failed to read sender.toml");
        toml::from_str(&content).expect("Failed to parse sender.toml")
    } else {
        println!("✨ Initializing default sender configuration...");
        let default_sender: SenderConfig = toml::from_str(DEFAULT_SENDER_TEMPLATE).expect("Failed to parse default sender.toml");
        fs::write(&path, DEFAULT_SENDER_TEMPLATE).expect("Failed to write sender.toml");
        default_sender
    }
}

fn setup_config_wizard() -> AppSettings {
    println!("\n⚙️  --- Configuration Setup ---");
    let current = load_settings();
    let default_val = current.map(|s| s.data_root).unwrap_or_else(|| "~/Documents/Business".to_string());

    println!("📂 Opening folder picker...");
    let picked_path = rfd::FileDialog::new()
        .set_title("Select Root Data Directory")
        .pick_folder();

    let new_root = if let Some(path) = picked_path {
        path.to_string_lossy().to_string()
    } else {
        println!("❌ No folder selected. Falling back to manual input.");
        Text::new("Enter Root Data Directory:").with_default(&default_val).prompt().unwrap()
    };

    let settings = AppSettings { data_root: new_root };
    
    let path = get_config_path();
    let toml_str = toml::to_string_pretty(&settings).unwrap();
    fs::write(&path, toml_str).expect("Failed to save settings");
    println!("✅ Settings saved.");
    settings
}

fn expand_home_dir(path: &str) -> String {
    if path.starts_with("~") {
        if let Some(base_dirs) = BaseDirs::new() {
            let home = base_dirs.home_dir().to_string_lossy();
            return path.replacen("~", &home, 1);
        }
    }
    path.to_string()
}

// Helper: Open file and reveal in Finder/Explorer
fn open_and_reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    Command::new("open").arg("-R").arg(path).spawn().ok();

    #[cfg(target_os = "windows")]
    Command::new("explorer").arg(format!("/select,{}", path.to_string_lossy())).spawn().ok();
    
    #[cfg(target_os = "linux")]
    Command::new("xdg-open").arg(path.parent().unwrap()).spawn().ok();

    #[cfg(target_os = "macos")]
    Command::new("open").arg(path).spawn().ok();

    #[cfg(target_os = "windows")]
    Command::new("explorer").arg(path).spawn().ok();

    #[cfg(target_os = "linux")]
    Command::new("xdg-open").arg(path).spawn().ok();
}

// ==========================================
// 8. Summary Logic
// ==========================================

struct InvoiceInfo {
    date: NaiveDate,
    total: f64,
    is_paid: bool,
    client: String,
}

fn show_summary(root: &Path, year: Option<i32>) {
    let output_dir = root.join("output");
    if !output_dir.exists() {
        println!("❌ No output directory found. No invoices to summarize.");
        return;
    }

    let target_year = year.unwrap_or_else(|| Local::now().year());
    println!("🔍 Scanning invoices for summary (Year: {})...", target_year);

    // 1. Recursively find all .typ files
    let mut typ_files = Vec::new();
    let mut stack = vec![output_dir];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().map_or(false, |e| e == "typ") {
                    // Exclude VOID invoices from summary
                    if !path.file_stem().unwrap().to_string_lossy().ends_with("_VOID") {
                        typ_files.push(path);
                    }
                }
            }
        }
    }

    if typ_files.is_empty() {
        println!("No invoices found.");
        return;
    }

    // 2. Parse date and total amount for each file
    let mut invoice_infos: Vec<InvoiceInfo> = Vec::new();
    let date_re = Regex::new(r"HI(\d{8})").unwrap();

    for path in typ_files {
        let filename = path.file_name().unwrap().to_string_lossy();
        
        if let Some(caps) = date_re.captures(&filename) {
            let date_str = &caps[1];
            if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y%m%d") {
                if let Ok((total, is_paid, client)) = parse_invoice_total(&path) {
                    invoice_infos.push(InvoiceInfo { date, total, is_paid, client });
                }
            }
        }
    }

    // 3. Group by month and calculate totals
    // Key: (Year, Month), Value: (Paid, Unpaid)
    let mut monthly_totals: BTreeMap<(i32, u32), (f64, f64)> = BTreeMap::new();
    // Key: Client Name, Value: (Paid, Unpaid)
    let mut client_totals: BTreeMap<String, (f64, f64)> = BTreeMap::new();

    for info in invoice_infos.iter().filter(|i| i.date.year() == target_year) {
        // Monthly Aggregation
        let month_key = (info.date.year(), info.date.month());
        let entry = monthly_totals.entry(month_key).or_insert((0.0, 0.0));
        if info.is_paid {
            entry.0 += info.total;
        } else {
            entry.1 += info.total;
        }

        // Client Aggregation
        let client_entry = client_totals.entry(info.client.clone()).or_insert((0.0, 0.0));
        if info.is_paid {
            client_entry.0 += info.total;
        } else {
            client_entry.1 += info.total;
        }
    }

    // 4. Create table using comfy-table (Monthly)
    let mut table = Table::new();
    table.set_header(vec![
        Cell::new("Month"),
        Cell::new("Paid"),
        Cell::new("Unpaid"),
        Cell::new("Total"),
    ]);

    let mut total_paid = 0.0;
    let mut total_unpaid = 0.0;

    for ((year, month), (paid, unpaid)) in monthly_totals.iter().rev() {
        let month_str = NaiveDate::from_ymd_opt(*year, *month, 1).unwrap().format("%B %Y").to_string();
        let total = paid + unpaid;

        let unpaid_cell = if *unpaid > 0.0 {
            Cell::new(format!("${:.2}", unpaid)).fg(Color::Rgb { r: 185, g: 28, b: 28 })
        } else {
            Cell::new(format!("${:.2}", unpaid))
        };

        let paid_cell = if *paid > 0.0 {
            Cell::new(format!("${:.2}", paid)).fg(Color::Rgb { r: 4, g: 120, b: 87 })
        } else {
            Cell::new(format!("${:.2}", paid))
        };

        table.add_row(vec![
            Cell::new(month_str),
            paid_cell,
            unpaid_cell,
            Cell::new(format!("${:.2}", total)),
        ]);
        total_paid += paid;
        total_unpaid += unpaid;
    }

    let total_unpaid_cell = Cell::new(format!("${:.2}", total_unpaid)).add_attribute(Attribute::Bold);
    let total_unpaid_cell = if total_unpaid > 0.0 {
        total_unpaid_cell.fg(Color::Rgb { r: 185, g: 28, b: 28 })
    } else {
        total_unpaid_cell
    };

    let total_paid_cell = Cell::new(format!("${:.2}", total_paid)).add_attribute(Attribute::Bold);
    let total_paid_cell = if total_paid > 0.0 {
        total_paid_cell.fg(Color::Rgb { r: 4, g: 120, b: 87 })
    } else {
        total_paid_cell
    };

    table.add_row(vec![
        Cell::new(format!("Total ({})", target_year)).add_attribute(Attribute::Bold),
        total_paid_cell,
        total_unpaid_cell,
        Cell::new(format!("${:.2}", total_paid + total_unpaid)).add_attribute(Attribute::Bold),
    ]);

    println!("\n--- Monthly Invoice Summary ({}) ---", target_year);
    println!("{table}");

    // 5. Client Summary Table
    let mut client_table = Table::new();
    client_table.set_header(vec![
        Cell::new("Client"),
        Cell::new("Paid"),
        Cell::new("Unpaid"),
        Cell::new("Total"),
    ]);

    // Sort clients by total amount descending
    let mut client_vec: Vec<_> = client_totals.into_iter().collect();
    client_vec.sort_by(|a, b| (b.1.0 + b.1.1).partial_cmp(&(a.1.0 + a.1.1)).unwrap());

    for (client, (paid, unpaid)) in client_vec {
        let total = paid + unpaid;

        let unpaid_cell = if unpaid > 0.0 {
            Cell::new(format!("${:.2}", unpaid)).fg(Color::Rgb { r: 185, g: 28, b: 28 })
        } else {
            Cell::new(format!("${:.2}", unpaid))
        };

        let paid_cell = if paid > 0.0 {
            Cell::new(format!("${:.2}", paid)).fg(Color::Rgb { r: 4, g: 120, b: 87 })
        } else {
            Cell::new(format!("${:.2}", paid))
        };

        client_table.add_row(vec![
            Cell::new(client),
            paid_cell,
            unpaid_cell,
            Cell::new(format!("${:.2}", total)),
        ]);
    }

    println!("\n--- Client Summary ({}) ---", target_year);
    println!("{client_table}");
}


fn parse_invoice_total(path: &Path) -> Result<(f64, bool, String), std::io::Error> {
    let content = fs::read_to_string(path)?;

    // Use global search for amount and tax_rate, which is more robust
    let amount_re = Regex::new(r#"amount:\s*([\d\.]+)"#).unwrap();
    let tax_re = Regex::new(r"tax_rate:\s*([\d\.]+)").unwrap();
    let paid_re = Regex::new(r"is_paid:\s*(true|false)").unwrap();
    let client_re = Regex::new(r#"client:\s*\(\s*name:\s*"([^"]+)""#).unwrap();

    let mut subtotal = 0.0;

    // Sum all amounts found in the file
    for cap in amount_re.captures_iter(&content) {
        if let Ok(amount) = cap[1].parse::<f64>() {
            subtotal += amount;
        }
    }
    
    // Get tax_rate
    let tax_rate = if let Some(tax_cap) = tax_re.captures(&content) {
        tax_cap[1].parse::<f64>().unwrap_or(0.0)
    } else {
        0.0
    };

    // Get is_paid status
    let is_paid = if let Some(paid_cap) = paid_re.captures(&content) {
        &paid_cap[1] == "true"
    } else {
        false
    };

    // Get client name
    let client_name = if let Some(client_cap) = client_re.captures(&content) {
        client_cap[1].replace("Attn:", "").trim().to_string()
    } else {
        "Unknown Client".to_string()
    };

    Ok((subtotal * (1.0 + tax_rate), is_paid, client_name))
}
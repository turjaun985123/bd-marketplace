//! The demo catalogue's data (Wave 57).
//!
//! Eight demo stores and their products, defined as
//! compact data tables — one line per product through
//! the [`p`] helper — and consumed by
//! [`crate::demo_seed`], which builds a CSV sheet per
//! store from these tables and runs it through the real
//! bulk-import service. Nothing here touches the
//! database; nothing here invents a taxonomy slug (every
//! `category_slug` names a node of the Wave 56 seed,
//! migration 0061 — the seed integration test pins that).
//!
//! The products are realistic Bangladesh catalogue
//! entries: Bangla + English titles, poisha prices a real
//! seller would charge, plausible weights and stock. The
//! catalogue is demo data and says so: the store names
//! carry no real business's name, and the seed only ever
//! runs behind the worker's `DEMO_SEED=1` guard.

/// One demo store.
pub struct DemoStoreDef {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// The seller account's phone (canonical +880 form).
    /// The eight numbers are sequential demo numbers in the
    /// 0171 range, used by no real subscriber-facing flow
    /// (the seed refuses to run outside its env guard).
    pub seller_phone: &'static str,
    pub seller_name: &'static str,
    pub theme_key: &'static str,
    pub brand_color: &'static str,
    pub announcement: &'static str,
}

/// One demo product. Prices are poisha, as everywhere.
pub struct DemoProductDef {
    pub sku: &'static str,
    pub title: &'static str,
    pub title_bn: &'static str,
    pub price_poisha: i64,
    pub compare_at_poisha: Option<i64>,
    pub stock: i32,
    pub weight_grams: i64,
    pub category_slug: &'static str,
    pub description: Option<&'static str>,
}

/// The one-line product constructor the tables below are
/// written with: sku, English title, Bangla title, price,
/// compare-at price, stock, weight in grams, category
/// slug, description.
#[allow(clippy::too_many_arguments)]
const fn p(
    sku: &'static str,
    title: &'static str,
    title_bn: &'static str,
    price_poisha: i64,
    compare_at_poisha: Option<i64>,
    stock: i32,
    weight_grams: i64,
    category_slug: &'static str,
    description: Option<&'static str>,
) -> DemoProductDef {
    DemoProductDef {
        sku,
        title,
        title_bn,
        price_poisha,
        compare_at_poisha,
        stock,
        weight_grams,
        category_slug,
        description,
    }
}

/// One store and its product table.
pub struct DemoStoreCatalogue {
    pub store: DemoStoreDef,
    pub products: &'static [DemoProductDef],
}

const FASHION_STORE: DemoStoreDef = DemoStoreDef {
    slug: "dhaka-fashion-demo",
    name: "ঢাকা ফ্যাশন হাউস (ডেমো)",
    description: "ডেমো স্টোর — ঈদ ও দৈনন্দিন পোশাকের সংগ্রহ।",
    seller_phone: "+8801710000001",
    seller_name: "Demo Fashion Seller",
    theme_key: "classic",
    brand_color: "#8E244D",
    announcement: "ঈদ কালেকশনে ২০% পর্যন্ত ছাড় চলছে",
};

const FASHION_PRODUCTS: &[DemoProductDef] = &[
    p("DF-PANJABI-001", "Premium Cotton Panjabi — Maroon", "প্রিমিয়াম সুতি পাঞ্জাবি — মেরুন", 185_000, Some(220_000), 25, 400, "panjabi", Some("Combed cotton, embroidered collar, festive fit.")),
    p("DF-PANJABI-002", "Handloom Jamdani-Motif Panjabi", "হাতে বোনা জামদানি মোটিফ পাঞ্জাবি", 245_000, None, 12, 420, "panjabi", Some("Tangail handloom weave with jamdani motifs.")),
    p("DF-PANJABI-003", "Kids Panjabi Set (Age 6-8)", "শিশু পাঞ্জাবি সেট (৬-৮ বছর)", 95_000, Some(120_000), 40, 250, "panjabi", Some("Soft cotton set with pajama, gentle on skin.")),
    p("DF-TEE-001", "Graphic T-Shirt — Dhaka Rickshaw Art", "গ্রাফিক টি-শার্ট — রিকশা আর্ট", 65_000, None, 80, 220, "shirts-t-shirts", Some("180 GSM cotton, rickshaw-art chest print.")),
    p("DF-TEE-002", "Polo Shirt — Navy", "পোলো শার্ট — নেভি", 89_000, Some(105_000), 55, 260, "shirts-t-shirts", Some("Pique knit polo with ribbed collar.")),
    p("DF-TEE-003", "Formal Shirt — White Oxford", "ফরমাল শার্ট — সাদা অক্সফোর্ড", 115_000, None, 35, 280, "shirts-t-shirts", Some("Wrinkle-resistant oxford weave, slim fit.")),
    p("DF-SAREE-001", "Jamdani Saree — Ivory & Gold", "জামদানি শাড়ি — হাতির দাঁত ও সোনালি", 550_000, Some(650_000), 8, 600, "saree", Some("Authentic Dhakai jamdani, handwoven in Narayanganj.")),
    p("DF-SAREE-002", "Tangail Cotton Saree — Teal", "টাঙ্গাইল সুতি শাড়ি — টিল", 185_000, None, 20, 550, "saree", Some("Everyday cotton saree with woven border.")),
    p("DF-SAREE-003", "Muslin Saree — Rose", "মসলিন শাড়ি — গোলাপি", 890_000, Some(990_000), 4, 480, "saree", Some("Fine muslin with zari work, blouse piece included.")),
    p("DF-SALWAR-001", "Three-Piece Salwar Kameez — Mint", "থ্রি-পিস সালোয়ার কামিজ — পুদিনা", 165_000, Some(195_000), 30, 500, "salwar-kameez", Some("Georgette kameez with embroidered neckline.")),
    p("DF-SALWAR-002", "Cotton Salwar Kameez — Mustard", "সুতি সালোয়ার কামিজ — সরিষা", 125_000, None, 42, 480, "salwar-kameez", Some("Breathable cotton set for daily wear.")),
    p("DF-MEN-001", "Men's Denim Jeans — Indigo", "পুরুষের ডেনিম জিন্স — ইন্ডিগো", 145_000, Some(175_000), 38, 650, "mens-fashion", Some("Stretch denim, tapered fit, sizes 30-38.")),
    p("DF-MEN-002", "Men's Chino Trousers — Khaki", "পুরুষের চিনো ট্রাউজার — খাকি", 119_000, None, 27, 520, "mens-fashion", Some("Cotton twill chinos with two back pockets.")),
    p("DF-MEN-003", "Men's Winter Hoodie — Charcoal", "পুরুষের শীতের হুডি — চারকোল", 135_000, Some(160_000), 22, 700, "mens-fashion", Some("Brushed fleece hoodie, kangaroo pocket.")),
    p("DF-WOMEN-001", "Women's Kurti — Floral Print", "মহিলাদের কুর্তি — ফুলের ছাপ", 98_000, None, 48, 350, "womens-fashion", Some("Rayon kurti, three-quarter sleeves.")),
    p("DF-WOMEN-002", "Women's Abaya — Black Crepe", "মহিলাদের আবায়া — কালো ক্রেপ", 210_000, Some(250_000), 15, 620, "womens-fashion", Some("Flowing crepe abaya with button front.")),
    p("DF-SHOE-001", "Leather Sandals — Tan", "চামড়ার স্যান্ডেল — ট্যান", 125_000, None, 33, 600, "footwear", Some("Genuine leather upper, cushioned sole.")),
    p("DF-SHOE-002", "Running Shoes — Grey/Orange", "রানিং জুতা — ধূসর/কমলা", 245_000, Some(295_000), 26, 800, "footwear", Some("Mesh upper with EVA midsole, sizes 39-44.")),
    p("DF-SHOE-003", "Kids School Shoes — Black", "শিশুদের স্কুলের জুতা — কালো", 85_000, None, 44, 450, "footwear", Some("Durable school shoes with velcro strap.")),
    p("DF-WATCH-001", "Men's Analog Watch — Leather Strap", "পুরুষের অ্যানালগ ঘড়ি — চামড়ার বেল্ট", 189_000, Some(240_000), 18, 180, "watches-accessories", Some("Quartz movement, water resistant to 3 ATM.")),
    p("DF-WATCH-002", "Women's Bracelet Watch — Rose Gold", "মহিলাদের ব্রেসলেট ঘড়ি — রোজ গোল্ড", 215_000, None, 11, 150, "watches-accessories", Some("Slim case with mesh bracelet.")),
    p("DF-ACC-001", "Leather Wallet — Brown Bifold", "চামড়ার মানিব্যাগ — বাদামি", 75_000, Some(95_000), 60, 120, "watches-accessories", Some("Full-grain leather, six card slots, RFID lining.")),
];

const GADGET_STORE: DemoStoreDef = DemoStoreDef {
    slug: "gadget-bazar-demo",
    name: "গ্যাজেট বাজার (ডেমো)",
    description: "ডেমো স্টোর — মোবাইল, অডিও ও কম্পিউটার গ্যাজেট।",
    seller_phone: "+8801710000002",
    seller_name: "Demo Gadget Seller",
    theme_key: "bold",
    brand_color: "#0D47A1",
    announcement: "সব গ্যাজেটে অফিসিয়াল ওয়ারেন্টি",
};

const GADGET_PRODUCTS: &[DemoProductDef] = &[
    p("GB-PHONE-001", "Android Smartphone 8/128GB — Ocean Blue", "অ্যান্ড্রয়েড স্মার্টফোন ৮/১২৮জিবি", 2_450_000, Some(2_650_000), 14, 420, "mobile-phones", Some("6.7-inch 120Hz display, 5000 mAh battery, official warranty.")),
    p("GB-PHONE-002", "Feature Phone with Torch — Dual SIM", "টর্চসহ ফিচার ফোন — ডুয়াল সিম", 185_000, None, 65, 180, "mobile-phones", Some("30-day standby, wireless FM radio, bright torch.")),
    p("GB-PHONE-003", "Android Smartphone 6/128GB — Graphite", "অ্যান্ড্রয়েড স্মার্টফোন ৬/১২৮জিবি", 1_890_000, Some(2_050_000), 19, 410, "mobile-phones", Some("50 MP main camera, 33W fast charging.")),
    p("GB-ACC-001", "USB-C Fast Charger 25W", "ইউএসবি-সি ফাস্ট চার্জার ২৫ওয়াট", 85_000, None, 120, 120, "mobile-accessories", Some("PD fast charging with 1m braided cable.")),
    p("GB-ACC-002", "Shockproof Phone Case — Clear", "শকপ্রুফ ফোন কেস — স্বচ্ছ", 35_000, Some(50_000), 200, 60, "mobile-accessories", Some("Raised edges, military-grade drop protection.")),
    p("GB-ACC-003", "Tempered Glass Screen Protector", "টেম্পার্ড গ্লাস স্ক্রিন প্রোটেক্টর", 25_000, None, 300, 40, "mobile-accessories", Some("9H hardness, alignment frame included.")),
    p("GB-ACC-004", "Power Bank 20000mAh", "পাওয়ার ব্যাংক ২০০০০mAh", 165_000, Some(195_000), 75, 350, "mobile-accessories", Some("22.5W two-way fast charge, LED indicator.")),
    p("GB-ACC-005", "Braided USB-C Cable 2m", "ব্রেইডেড ইউএসবি-সি কেবল ২মি", 30_000, None, 250, 50, "mobile-accessories", Some("60W rated nylon braid, 30000-bend tested.")),
    p("GB-LAP-001", "Laptop 15.6-inch Ryzen 5 / 16GB / 512GB", "ল্যাপটপ ১৫.৬ ইঞ্চি রাইজেন ৫", 6_850_000, Some(7_200_000), 5, 2_400, "laptops", Some("Full HD IPS, backlit keyboard, 2-year warranty.")),
    p("GB-LAP-002", "Ultrabook 14-inch Core i5 / 16GB / 512GB", "আল্ট্রাবুক ১৪ ইঞ্চি কোর আই৫", 8_950_000, None, 3, 1_900, "laptops", Some("1.29 kg aluminium body, fingerprint reader.")),
    p("GB-PC-001", "Desktop PC — Ryzen 5 / 16GB / 1TB", "ডেস্কটপ পিসি — রাইজেন ৫", 5_450_000, Some(5_800_000), 6, 9_000, "computers", Some("Assembled in-store, 3-year parts warranty.")),
    p("GB-PC-002", "Monitor 24-inch Full HD IPS", "মনিটর ২৪ ইঞ্চি ফুল এইচডি", 1_450_000, None, 16, 3_800, "computers", Some("75Hz, HDMI + VGA, low blue light mode.")),
    p("GB-PCACC-001", "Mechanical Keyboard — Hot-Swap", "মেকানিক্যাল কিবোর্ড — হট-সোয়াপ", 425_000, Some(480_000), 28, 850, "computer-accessories", Some("Gasket mount, PBT keycaps, tri-mode connect.")),
    p("GB-PCACC-002", "Wireless Mouse — Silent Click", "ওয়্যারলেস মাউস — নীরব ক্লিক", 95_000, None, 90, 110, "computer-accessories", Some("2.4G + Bluetooth, 18-month battery.")),
    p("GB-PCACC-003", "Laptop Cooling Pad — Dual Fan", "ল্যাপটপ কুলিং প্যাড — ডুয়াল ফ্যান", 135_000, Some(160_000), 40, 900, "computer-accessories", Some("Adjustable height, USB powered.")),
    p("GB-PCACC-004", "Webcam 1080p with Privacy Shutter", "ওয়েবক্যাম ১০৮০পি", 215_000, None, 22, 160, "computer-accessories", Some("Auto light correction, built-in microphone.")),
    p("GB-AUDIO-001", "True Wireless Earbuds — ENC", "ট্রু ওয়্যারলেস ইয়ারবাড", 199_000, Some(249_000), 85, 90, "audio", Some("36-hour case, ENC call noise reduction.")),
    p("GB-AUDIO-002", "Over-Ear Headphones — ANC", "ওভার-ইয়ার হেডফোন — ANC", 349_000, None, 31, 320, "audio", Some("Hybrid ANC, 60-hour playback, foldable.")),
    p("GB-AUDIO-003", "Bluetooth Speaker — 20W", "ব্লুটুথ স্পিকার — ২০ওয়াট", 225_000, Some(275_000), 47, 750, "audio", Some("IPX7 waterproof, 12-hour playtime.")),
    p("GB-AUDIO-004", "Wired Earphones — Type-C", "ওয়্যার্ড ইয়ারফোন — টাইপ-সি", 55_000, None, 180, 40, "audio", Some("In-line mic and volume control.")),
    p("GB-HOME-001", "Rechargeable Fan 16-inch", "রিচার্জেবল ফ্যান ১৬ ইঞ্চি", 385_000, Some(450_000), 24, 4_200, "home-appliances", Some("8-hour backup, remote control, LED light.")),
    p("GB-HOME-002", "Electric Kettle 1.8L", "ইলেকট্রিক কেটলি ১.৮ লিটার", 145_000, None, 58, 1_100, "home-appliances", Some("Stainless steel, auto shut-off, 1500W.")),
];

const GROCERY_STORE: DemoStoreDef = DemoStoreDef {
    slug: "shad-bazar-demo",
    name: "স্বাদ বাজার (ডেমো)",
    description: "ডেমো স্টোর — চাল, তেল, মসলা ও নিত্যপ্রয়োজনীয় বাজার।",
    seller_phone: "+8801710000003",
    seller_name: "Demo Grocery Seller",
    theme_key: "minimal",
    brand_color: "#2E7D32",
    announcement: "৫০০ টাকার বেশি অর্ডারে হোম ডেলিভারি সুবিধা",
};

const GROCERY_PRODUCTS: &[DemoProductDef] = &[
    p("SB-RICE-001", "Nazirshail Rice 5kg", "নাজিরশাইল চাল ৫ কেজি", 62_000, None, 120, 5_000, "rice-grains", Some("Aromatic fine rice from Dinajpur, aged one season.")),
    p("SB-RICE-002", "Basmati Rice 1kg", "বাসমতী চাল ১ কেজি", 21_500, Some(24_000), 200, 1_000, "rice-grains", Some("Long-grain basmati for biryani and polao.")),
    p("SB-RICE-003", "Miniket Rice 10kg", "মিনিকেট চাল ১০ কেজি", 98_000, None, 80, 10_000, "rice-grains", Some("Polished everyday rice, low breakage.")),
    p("SB-RICE-004", "Chinigura Rice 2kg", "চিনিগুঁড়া চাল ২ কেজি", 34_000, None, 90, 2_000, "rice-grains", Some("Fragrant short grain for payesh and khichuri.")),
    p("SB-RICE-005", "Red Lentil (Masoor Dal) 1kg", "মসুর ডাল ১ কেজি", 15_500, Some(17_000), 150, 1_000, "rice-grains", Some("Premium deshi masoor, stone-free.")),
    p("SB-RICE-006", "Mug Dal 500g", "মুগ ডাল ৫০০ গ্রাম", 11_000, None, 140, 500, "rice-grains", Some("Golden roasted mug dal.")),
    p("SB-SPICE-001", "Turmeric Powder 200g", "হলুদ গুঁড়া ২০০ গ্রাম", 9_500, None, 180, 200, "spices", Some("Ground from whole Bogura turmeric fingers.")),
    p("SB-SPICE-002", "Red Chilli Powder 200g", "লাল মরিচ গুঁড়া ২০০ গ্রাম", 11_500, Some(13_000), 160, 200, "spices", Some("Sun-dried chillies, no added colour.")),
    p("SB-SPICE-003", "Cumin Seeds 100g", "জিরা ১০০ গ্রাম", 14_000, None, 130, 100, "spices", Some("Whole jeera, high aroma.")),
    p("SB-SPICE-004", "Garam Masala 100g", "গরম মসলা ১০০ গ্রাম", 16_500, None, 110, 100, "spices", Some("Cardamom, cinnamon, clove and bay leaf blend.")),
    p("SB-SPICE-005", "Panch Phoron 100g", "পাঁচ ফোড়ন ১০০ গ্রাম", 6_500, None, 100, 100, "spices", Some("Five-spice mix for dal and vegetable tadka.")),
    p("SB-OIL-001", "Pure Mustard Oil 1L", "খাঁটি সরিষার তেল ১ লিটার", 32_000, Some(36_000), 95, 950, "edible-oil", Some("Cold-pressed ghani mustard oil, pungent and pure.")),
    p("SB-OIL-002", "Soybean Oil 2L", "সয়াবিন তেল ২ লিটার", 37_500, None, 110, 1_900, "edible-oil", Some("Refined soybean oil, vitamin A fortified.")),
    p("SB-OIL-003", "Extra Virgin Olive Oil 250ml", "এক্সট্রা ভার্জিন অলিভ অয়েল ২৫০মিলি", 58_000, None, 45, 480, "edible-oil", Some("First cold press, for salads and finishing.")),
    p("SB-OIL-004", "Ghee 500g", "ঘি ৫০০ গ্রাম", 72_000, Some(80_000), 60, 550, "edible-oil", Some("Pure cow ghee, grainy texture, from Pabna.")),
    p("SB-SNACK-001", "Chanachur 400g", "চানাচুর ৪০০ গ্রাম", 11_000, None, 170, 400, "snacks-drinks", Some("Spicy Bombay mix, tea-time favourite.")),
    p("SB-SNACK-002", "Roasted Makhana 100g", "ভাজা মাখানা ১০০ গ্রাম", 18_500, None, 75, 100, "snacks-drinks", Some("Lightly salted fox nuts, guilt-free snacking.")),
    p("SB-SNACK-003", "Green Tea 100g — Sylhet", "গ্রিন টি ১০০ গ্রাম — সিলেট", 24_000, Some(28_000), 85, 120, "snacks-drinks", Some("Whole-leaf green tea from Sylhet gardens.")),
    p("SB-SNACK-004", "Black Tea 500g — CTC", "কালো চা ৫০০ গ্রাম — সিটিসি", 32_000, None, 100, 500, "snacks-drinks", Some("Strong CTC blend for milk tea.")),
    p("SB-SNACK-005", "Mixed Dry Fruits 500g", "মিক্সড ড্রাই ফ্রুটস ৫০০ গ্রাম", 85_000, Some(95_000), 40, 500, "snacks-drinks", Some("Almond, cashew, raisin and walnut mix.")),
    p("SB-GROC-001", "Farm Eggs — 12 pcs", "ফার্মের ডিম — ১২টি", 16_800, None, 300, 700, "grocery", Some("Farm-fresh brown eggs, tray packed.")),
    p("SB-GROC-002", "Iodised Salt 1kg", "আয়োডিনযুক্ত লবণ ১ কেজি", 4_200, None, 400, 1_000, "grocery", Some("Refined iodised table salt.")),
];

const HOME_STORE: DemoStoreDef = DemoStoreDef {
    slug: "ghor-sajja-demo",
    name: "ঘর সাজ্জা (ডেমো)",
    description: "ডেমো স্টোর — আসবাব, রান্নাঘর ও ঘর সাজানোর সবকিছু।",
    seller_phone: "+8801710000004",
    seller_name: "Demo Home Seller",
    theme_key: "classic",
    brand_color: "#6D4C41",
    announcement: "আসবাবে ১ বছরের সার্ভিস ওয়ারেন্টি",
};

const HOME_PRODUCTS: &[DemoProductDef] = &[
    p("GS-FURN-001", "Wooden Bookshelf — 4 Tier", "কাঠের বুকশেলফ — ৪ তাক", 685_000, Some(750_000), 9, 18_000, "furniture", Some("Seasoned gamari wood, natural polish.")),
    p("GS-FURN-002", "Study Table with Drawer", "ড্রয়ারসহ পড়ার টেবিল", 545_000, None, 12, 15_000, "furniture", Some("Engineered wood, cable hole, one lockable drawer.")),
    p("GS-FURN-003", "Rocking Chair — Cane & Wood", "দোলনা চেয়ার — বেত ও কাঠ", 425_000, Some(480_000), 7, 8_500, "furniture", Some("Handwoven cane back, smooth rockers.")),
    p("GS-FURN-004", "Shoe Rack — 5 Layer", "জুতার র‌্যাক — ৫ স্তর", 185_000, None, 25, 6_000, "furniture", Some("Holds 15 pairs, dust-cover flap.")),
    p("GS-FURN-005", "Folding Dining Set — 4 Chairs", "ভাঁজযোগ্য ডাইনিং সেট — ৪ চেয়ার", 1_250_000, Some(1_400_000), 4, 32_000, "furniture", Some("Solid mehagoni finish, space-saving fold.")),
    p("GS-KIT-001", "Non-Stick Cookware Set — 7 pcs", "নন-স্টিক কুকওয়্যার সেট — ৭টি", 345_000, None, 20, 4_500, "kitchen", Some("Induction-ready, glass lids, cool-touch handles.")),
    p("GS-KIT-002", "Pressure Cooker 5L", "প্রেশার কুকার ৫ লিটার", 215_000, Some(250_000), 30, 2_800, "kitchen", Some("ISI-grade aluminium, extra gasket included.")),
    p("GS-KIT-003", "Stainless Steel Dinner Set — 32 pcs", "স্টেইনলেস স্টিল ডিনার সেট — ৩২টি", 485_000, None, 14, 5_200, "kitchen", Some("Food-grade steel for six settings.")),
    p("GS-KIT-004", "Knife Set with Block — 6 pcs", "ছুরি সেট ব্লকসহ — ৬টি", 165_000, Some(195_000), 36, 1_600, "kitchen", Some("High-carbon steel, ergonomic handles.")),
    p("GS-KIT-005", "Electric Rice Cooker 2.8L", "ইলেকট্রিক রাইস কুকার ২.৮ লিটার", 265_000, None, 22, 3_400, "kitchen", Some("Keep-warm mode, steamer basket included.")),
    p("GS-BED-001", "Cotton Bedsheet Set — King", "সুতি বিছানার চাদর সেট — কিং", 145_000, Some(175_000), 45, 900, "bedding", Some("144 TC cotton with two pillow covers.")),
    p("GS-BED-002", "Nakshi Kantha — Double", "নকশি কাঁথা — ডাবল", 225_000, None, 16, 1_400, "bedding", Some("Hand-embroidered in Jamalpur, traditional motifs.")),
    p("GS-BED-003", "Mosquito Net — King Size", "মশারি — কিং সাইজ", 65_000, None, 70, 500, "bedding", Some("Fine mesh with reinforced corners.")),
    p("GS-BED-004", "Memory Foam Pillow", "মেমোরি ফোম বালিশ", 95_000, Some(120_000), 38, 800, "bedding", Some("Contour support with washable cover.")),
    p("GS-LIGHT-001", "LED Table Lamp — Rechargeable", "এলইডি টেবিল ল্যাম্প — রিচার্জেবল", 85_000, None, 52, 600, "lighting", Some("Three colour temperatures, touch dimmer.")),
    p("GS-LIGHT-002", "Ceiling Light — Warm White 24W", "সিলিং লাইট — ওয়ার্ম হোয়াইট ২৪ওয়াট", 115_000, Some(140_000), 34, 900, "lighting", Some("Surface-mount panel, 2-year warranty.")),
    p("GS-LIGHT-003", "Fairy String Lights — 10m", "ফেয়ারি স্ট্রিং লাইট — ১০ মিটার", 45_000, None, 90, 200, "lighting", Some("100 warm LEDs, eight modes, USB powered.")),
    p("GS-HOME-001", "Wall Clock — Wooden 12-inch", "দেয়াল ঘড়ি — কাঠের ১২ ইঞ্চি", 95_000, None, 28, 700, "home-living", Some("Silent sweep movement, rustic finish.")),
    p("GS-HOME-002", "Indoor Plant Pot Set — 3 pcs", "ইনডোর গাছের টব সেট — ৩টি", 55_000, Some(70_000), 64, 1_100, "home-living", Some("Ceramic pots with bamboo trays.")),
    p("GS-HOME-003", "Storage Box Set — 4 pcs", "স্টোরেজ বাক্স সেট — ৪টি", 75_000, None, 48, 900, "home-living", Some("Foldable fabric boxes with lids.")),
];

const BEAUTY_STORE: DemoStoreDef = DemoStoreDef {
    slug: "rupayan-beauty-demo",
    name: "রূপায়ন বিউটি (ডেমো)",
    description: "ডেমো স্টোর — স্কিনকেয়ার, মেকআপ ও পার্সোনাল কেয়ার।",
    seller_phone: "+8801710000005",
    seller_name: "Demo Beauty Seller",
    theme_key: "minimal",
    brand_color: "#AD1457",
    announcement: "সব পণ্য ১০০% অরিজিনাল — মেয়াদ যাচাই করে কিনুন",
};

const BEAUTY_PRODUCTS: &[DemoProductDef] = &[
    p("RB-SKIN-001", "Vitamin C Serum 30ml", "ভিটামিন সি সিরাম ৩০মিলি", 85_000, Some(99_000), 60, 120, "skincare", Some("15% vitamin C with hyaluronic acid.")),
    p("RB-SKIN-002", "Sunscreen SPF 50 PA+++ 50ml", "সানস্ক্রিন SPF 50 PA+++ ৫০মিলি", 72_000, None, 85, 110, "skincare", Some("No white cast, suitable for humid weather.")),
    p("RB-SKIN-003", "Gentle Foaming Cleanser 150ml", "জেন্টল ফোমিং ক্লিনজার ১৫০মিলি", 58_000, Some(68_000), 74, 220, "skincare", Some("pH 5.5, for daily use on sensitive skin.")),
    p("RB-SKIN-004", "Moisturising Cream 100ml", "ময়েশ্চারাইজিং ক্রিম ১০০মিলি", 64_000, None, 66, 180, "skincare", Some("Ceramide-rich, fragrance free.")),
    p("RB-SKIN-005", "Aloe Vera Gel 200ml", "অ্যালো ভেরা জেল ২০০মিলি", 35_000, Some(45_000), 120, 260, "skincare", Some("99% aloe, soothes sun-exposed skin.")),
    p("RB-SKIN-006", "Clay Face Mask 100g", "ক্লে ফেস মাস্ক ১০০ গ্রাম", 42_000, None, 58, 150, "skincare", Some("Multani mitti blend for oily skin.")),
    p("RB-MAKE-001", "Matte Lipstick — Brick Red", "ম্যাট লিপস্টিক — ইট লাল", 45_000, None, 95, 40, "makeup", Some("8-hour matte, vitamin E enriched.")),
    p("RB-MAKE-002", "Liquid Foundation — Medium", "লিকুইড ফাউন্ডেশন — মিডিয়াম", 88_000, Some(105_000), 44, 90, "makeup", Some("Medium coverage, natural finish.")),
    p("RB-MAKE-003", "Kajal Eyeliner — Jet Black", "কাজল আইলাইনার — জেট ব্ল্যাক", 22_000, None, 150, 25, "makeup", Some("Smudge-proof, 12-hour wear.")),
    p("RB-MAKE-004", "Compact Powder — Natural Beige", "কমপ্যাক্ট পাউডার — ন্যাচারাল বেইজ", 52_000, Some(62_000), 70, 80, "makeup", Some("Oil control with mirror and puff.")),
    p("RB-MAKE-005", "Blush Palette — 4 Shades", "ব্লাশ প্যালেট — ৪ শেড", 68_000, None, 39, 130, "makeup", Some("Buildable pigment, matte and shimmer.")),
    p("RB-CARE-001", "Argan Hair Oil 100ml", "আরগান হেয়ার অয়েল ১০০মিলি", 55_000, Some(65_000), 88, 160, "personal-care", Some("Cold-pressed argan, frizz control.")),
    p("RB-CARE-002", "Herbal Shampoo 340ml", "হারবাল শ্যাম্পু ৩৪০মিলি", 38_000, None, 102, 420, "personal-care", Some("Amla and shikakai, sulphate free.")),
    p("RB-CARE-003", "Body Lotion 400ml — Cocoa", "বডি লোশন ৪০০মিলি — কোকো", 46_000, None, 77, 480, "personal-care", Some("48-hour moisture for dry skin.")),
    p("RB-CARE-004", "Charcoal Toothpaste 120g", "চারকোল টুথপেস্ট ১২০ গ্রাম", 18_500, None, 140, 150, "personal-care", Some("Activated charcoal, fluoride free.")),
    p("RB-CARE-005", "Hand Cream 50ml — Shea", "হ্যান্ড ক্রিম ৫০মিলি — শিয়া", 28_000, Some(35_000), 92, 70, "personal-care", Some("Fast absorbing, non greasy.")),
    p("RB-BEAUTY-001", "Makeup Brush Set — 12 pcs", "মেকআপ ব্রাশ সেট — ১২টি", 75_000, None, 41, 300, "beauty-health", Some("Synthetic fibres with pouch.")),
    p("RB-BEAUTY-002", "Facial Steamer — Nano Ionic", "ফেসিয়াল স্টিমার — ন্যানো আয়নিক", 185_000, Some(220_000), 18, 900, "beauty-health", Some("10-minute steam, opens pores gently.")),
    p("RB-BEAUTY-003", "Electric Trimmer — Bikini Line", "ইলেকট্রিক ট্রিমার", 125_000, None, 26, 220, "beauty-health", Some("Waterproof, USB rechargeable.")),
    p("RB-BEAUTY-004", "Jade Roller & Gua Sha Set", "জেড রোলার ও গুয়া শা সেট", 38_000, Some(48_000), 63, 180, "beauty-health", Some("Natural stone, cooling massage.")),
];

const AGRO_STORE: DemoStoreDef = DemoStoreDef {
    slug: "krishi-bangla-demo",
    name: "কৃষি বাংলা (ডেমো)",
    description: "ডেমো স্টোর — বীজ, সার ও কৃষি যন্ত্রপাতি।",
    seller_phone: "+8801710000006",
    seller_name: "Demo Agro Seller",
    theme_key: "classic",
    brand_color: "#33691E",
    announcement: "বীজের অঙ্কুরোদগম হার পরীক্ষিত",
};

const AGRO_PRODUCTS: &[DemoProductDef] = &[
    p("KB-SEED-001", "Hybrid Tomato Seeds 10g", "হাইব্রিড টমেটো বীজ ১০ গ্রাম", 18_500, None, 90, 30, "seeds", Some("High-yield hybrid, germination tested above 85%.")),
    p("KB-SEED-002", "Lal Shak Seeds 50g", "লাল শাক বীজ ৫০ গ্রাম", 6_500, None, 200, 60, "seeds", Some("Fast-growing red amaranth for homestead plots.")),
    p("KB-SEED-003", "Chilli Seeds — Bombay Morich 5g", "মরিচ বীজ — বোম্বাই মরিচ ৫ গ্রাম", 12_000, Some(15_000), 140, 20, "seeds", Some("Fiery naga-type chilli, handle with care.")),
    p("KB-SEED-004", "Cucumber Seeds 10g", "শসা বীজ ১০ গ্রাম", 9_500, None, 160, 30, "seeds", Some("Disease-tolerant deshi cucumber.")),
    p("KB-SEED-005", "BRRI Dhan 28 Paddy Seeds 10kg", "ব্রি ধান ২৮ বীজ ১০ কেজি", 65_000, None, 55, 10_000, "seeds", Some("Certified boro paddy seed, BRRI variety.")),
    p("KB-SEED-006", "Mustard Seeds 1kg — Bari-14", "সরিষা বীজ ১ কেজি — বারি-১৪", 22_000, None, 80, 1_000, "seeds", Some("High oil content mustard for robi season.")),
    p("KB-FERT-001", "Urea Fertilizer 5kg", "ইউরিয়া সার ৫ কেজি", 14_500, None, 120, 5_000, "fertilizer", Some("46% nitrogen, granular, sealed pack.")),
    p("KB-FERT-002", "TSP Fertilizer 2kg", "টিএসপি সার ২ কেজি", 11_000, None, 100, 2_000, "fertilizer", Some("Triple super phosphate for root growth.")),
    p("KB-FERT-003", "Organic Compost 10kg", "জৈব কম্পোস্ট ১০ কেজি", 25_000, Some(30_000), 70, 10_000, "fertilizer", Some("Vermicompost, ready for beds and pots.")),
    p("KB-FERT-004", "Liquid Seaweed Fertilizer 500ml", "তরল সিউইড সার ৫০০মিলি", 32_000, None, 48, 600, "fertilizer", Some("Foliar spray concentrate, 1:200 dilution.")),
    p("KB-TOOL-001", "Hand Trowel & Fork Set", "হ্যান্ড ট্রোয়েল ও ফর্ক সেট", 28_500, None, 65, 450, "farm-tools", Some("Stainless heads with ash wood handles.")),
    p("KB-TOOL-002", "Pruning Shears — Bypass", "ছাঁটাই কাঁচি — বাইপাস", 45_000, Some(55_000), 52, 380, "farm-tools", Some("SK5 steel blade, cuts up to 20mm.")),
    p("KB-TOOL-003", "Knapsack Sprayer 16L", "ন্যাপস্যাক স্প্রেয়ার ১৬ লিটার", 185_000, None, 21, 3_600, "farm-tools", Some("Manual pressure sprayer with 4 nozzles.")),
    p("KB-TOOL-004", "Watering Can 10L — Galvanised", "ঝাঁঝরি ১০ লিটার — গ্যালভানাইজড", 68_000, None, 33, 1_500, "farm-tools", Some("Removable rose head, rust resistant.")),
    p("KB-TOOL-005", "Hoe (Kodal) with Handle", "কোদাল হাতলসহ", 55_000, Some(65_000), 44, 2_200, "farm-tools", Some("Forged steel blade, seasoned wood handle.")),
    p("KB-AGRO-001", "Mulching Film 100m Roll", "মালচিং ফিল্ম ১০০ মিটার রোল", 145_000, None, 26, 4_800, "agriculture", Some("25-micron black film for weed control.")),
    p("KB-AGRO-002", "Drip Irrigation Kit — 100 Plants", "ড্রিপ সেচ কিট — ১০০ গাছ", 225_000, Some(260_000), 15, 3_200, "agriculture", Some("Complete kit with mainline, drippers and filter.")),
    p("KB-AGRO-003", "Pheromone Trap Set — 5 pcs", "ফেরোমন ফাঁদ সেট — ৫টি", 35_000, None, 58, 400, "agriculture", Some("Fruit-fly lure traps for mango and guava.")),
];

const SPORTS_STORE: DemoStoreDef = DemoStoreDef {
    slug: "khela-ghor-demo",
    name: "খেলাঘর স্পোর্টস (ডেমো)",
    description: "ডেমো স্টোর — ক্রিকেট, ফুটবল ও ব্যায়ামের সরঞ্জাম।",
    seller_phone: "+8801710000007",
    seller_name: "Demo Sports Seller",
    theme_key: "bold",
    brand_color: "#EF6C00",
    announcement: "ক্রিকেট সিজনে ব্যাটে বিশেষ ছাড়",
};

const SPORTS_PRODUCTS: &[DemoProductDef] = &[
    p("KG-CRIC-001", "English Willow Cricket Bat — SH", "ইংলিশ উইলো ক্রিকেট ব্যাট", 850_000, Some(950_000), 9, 1_150, "cricket", Some("Grade-2 willow, cane handle, toe guard fitted.")),
    p("KG-CRIC-002", "Kashmir Willow Bat — Tennis Ball", "কাশ্মীর উইলো ব্যাট — টেনিস বল", 185_000, None, 26, 1_050, "cricket", Some("For tape-tennis cricket, knocked-in ready.")),
    p("KG-CRIC-003", "Cricket Ball — Leather 4pc (2 pcs)", "ক্রিকেট বল — চামড়া ৪ খণ্ড (২টি)", 95_000, Some(120_000), 60, 320, "cricket", Some("Alum-tanned leather, hand stitched seam.")),
    p("KG-CRIC-004", "Batting Gloves — Right Hand", "ব্যাটিং গ্লাভস — ডান হাত", 145_000, None, 34, 380, "cricket", Some("Cotton palm with fibre shields.")),
    p("KG-CRIC-005", "Cricket Helmet — Steel Grill", "ক্রিকেট হেলমেট — স্টিল গ্রিল", 265_000, Some(310_000), 17, 950, "cricket", Some("Adjustable dial fit, sizes M and L.")),
    p("KG-CRIC-006", "Cricket Kit Bag — Wheelie", "ক্রিকেট কিট ব্যাগ — চাকাসহ", 225_000, None, 21, 1_800, "cricket", Some("Separate bat cave, two end pockets.")),
    p("KG-FOOT-001", "Football — Size 5 Machine Stitched", "ফুটবল — সাইজ ৫", 115_000, Some(140_000), 48, 450, "football", Some("All-weather TPU cover, butyl bladder.")),
    p("KG-FOOT-002", "Football Boots — Firm Ground", "ফুটবল বুট — ফার্ম গ্রাউন্ড", 295_000, None, 23, 780, "football", Some("Synthetic upper, moulded studs, sizes 39-44.")),
    p("KG-FOOT-003", "Goalkeeper Gloves — Latex Palm", "গোলকিপার গ্লাভস — ল্যাটেক্স", 135_000, Some(160_000), 29, 260, "football", Some("Finger protection spines, wrist strap.")),
    p("KG-FOOT-004", "Team Jersey Set — 10 pcs", "টিম জার্সি সেট — ১০টি", 650_000, None, 12, 2_200, "football", Some("Breathable mesh with numbers printed.")),
    p("KG-EXER-001", "Adjustable Dumbbell 10kg Pair", "অ্যাডজাস্টেবল ডাম্বেল ১০ কেজি জোড়া", 385_000, Some(450_000), 19, 10_500, "exercise-equipment", Some("Cast-iron plates with chrome bars.")),
    p("KG-EXER-002", "Yoga Mat 6mm — Anti Skid", "যোগা ম্যাট ৬মিমি — অ্যান্টি স্কিড", 85_000, None, 57, 900, "exercise-equipment", Some("TPE foam with carry strap, 183x61cm.")),
    p("KG-EXER-003", "Resistance Bands Set — 5 Levels", "রেজিস্ট্যান্স ব্যান্ড সেট — ৫ লেভেল", 55_000, Some(70_000), 84, 300, "exercise-equipment", Some("Natural latex loops with pouch.")),
    p("KG-EXER-004", "Skipping Rope — Bearing Handle", "স্কিপিং রোপ — বিয়ারিং হাতল", 35_000, None, 96, 180, "exercise-equipment", Some("Adjustable steel cable, foam grips.")),
    p("KG-EXER-005", "Push-Up Boards Pair", "পুশ-আপ বোর্ড জোড়া", 65_000, Some(85_000), 43, 700, "exercise-equipment", Some("Colour-coded muscle targets, foldable.")),
    p("KG-SPORT-001", "Badminton Racket — Carbon Shaft", "ব্যাডমিন্টন র‌্যাকেট — কার্বন শ্যাফট", 165_000, None, 31, 240, "sports-fitness", Some("Pre-strung at 24 lbs with full cover.")),
    p("KG-SPORT-002", "Shuttlecock Tube — Feather (12 pcs)", "শাটলকক টিউব — পালক (১২টি)", 78_000, Some(92_000), 66, 220, "sports-fitness", Some("Goose feather, speed 77.")),
    p("KG-SPORT-003", "Sports Water Bottle 1L", "স্পোর্টস পানির বোতল ১ লিটার", 28_000, None, 120, 160, "sports-fitness", Some("BPA-free with flip straw lid.")),
];

const BABY_STORE: DemoStoreDef = DemoStoreDef {
    slug: "shishu-bazar-demo",
    name: "শিশু বাজার (ডেমো)",
    description: "ডেমো স্টোর — শিশুর খাবার, পোশাক ও খেলনা।",
    seller_phone: "+8801710000008",
    seller_name: "Demo Baby Seller",
    theme_key: "minimal",
    brand_color: "#00838F",
    announcement: "শিশু পণ্যে মেয়াদ ও নিরাপত্তা যাচাইকৃত",
};

const BABY_PRODUCTS: &[DemoProductDef] = &[
    p("SH-FOOD-001", "Infant Formula Stage 1 — 400g", "শিশু ফর্মুলা স্টেজ ১ — ৪০০ গ্রাম", 145_000, None, 36, 480, "baby-food", Some("For 0-6 months; follow preparation guidance on pack.")),
    p("SH-FOOD-002", "Baby Cereal — Rice & Milk 300g", "শিশু সিরিয়াল — চাল ও দুধ ৩০০ গ্রাম", 58_000, Some(65_000), 64, 360, "baby-food", Some("Iron fortified, for 6+ months.")),
    p("SH-FOOD-003", "Organic Oats Porridge 250g", "অর্গানিক ওটস পোরিজ ২৫০ গ্রাম", 46_000, None, 52, 300, "baby-food", Some("No added sugar, finely milled.")),
    p("SH-FOOD-004", "Fruit Puree Pouch — Mango 90g", "ফলের পিউরি পাউচ — আম ৯০ গ্রাম", 18_500, None, 110, 110, "baby-food", Some("100% fruit, no preservatives.")),
    p("SH-DIA-001", "Baby Diapers — M (7-12kg) 50 pcs", "শিশু ডায়াপার — M ৫০টি", 125_000, Some(145_000), 48, 1_600, "diapers", Some("12-hour absorption, breathable backsheet.")),
    p("SH-DIA-002", "Baby Diapers — L (9-14kg) 44 pcs", "শিশু ডায়াপার — L ৪৪টি", 125_000, None, 44, 1_550, "diapers", Some("Wetness indicator, elastic waistband.")),
    p("SH-DIA-003", "Baby Wipes — 80 pcs Unscented", "শিশু ওয়াইপস — ৮০টি", 22_000, Some(28_000), 130, 420, "diapers", Some("99% water, alcohol free.")),
    p("SH-DIA-004", "Swim Diapers — Reusable", "সাঁতারের ডায়াপার — পুনঃব্যবহারযোগ্য", 45_000, None, 39, 120, "diapers", Some("Adjustable snaps, washable mesh lining.")),
    p("SH-KID-001", "Kids Cotton T-Shirt — Dinosaur", "শিশুদের সুতি টি-শার্ট — ডাইনোসর", 45_000, None, 78, 150, "kids-fashion", Some("Soft combed cotton, ages 2-8.")),
    p("SH-KID-002", "Kids Frock — Floral (Age 4-6)", "শিশুদের ফ্রক — ফুলের (৪-৬ বছর)", 85_000, Some(99_000), 41, 280, "kids-fashion", Some("Cotton-lined bodice with sash belt.")),
    p("SH-KID-003", "Kids Pajama Set — Space Print", "শিশুদের পাজামা সেট — মহাকাশ ছাপ", 62_000, None, 59, 320, "kids-fashion", Some("Breathable knit, ages 3-10.")),
    p("SH-KID-004", "Kids Raincoat — Hooded", "শিশুদের রেইনকোট — হুডসহ", 75_000, Some(90_000), 33, 350, "kids-fashion", Some("Waterproof PVC with reflective strip.")),
    p("SH-TOY-001", "Wooden Building Blocks — 50 pcs", "কাঠের বিল্ডিং ব্লক — ৫০টি", 95_000, None, 27, 1_100, "toys", Some("Non-toxic paints, storage bag included.")),
    p("SH-TOY-002", "Remote Control Car — Stunt", "রিমোট কন্ট্রোল গাড়ি — স্টান্ট", 145_000, Some(175_000), 24, 650, "toys", Some("360-degree flips, USB rechargeable.")),
    p("SH-TOY-003", "Plush Teddy Bear — 40cm", "প্লাশ টেডি বিয়ার — ৪০ সেমি", 68_000, None, 46, 380, "toys", Some("Washable plush with stitched eyes.")),
    p("SH-TOY-004", "Bengali Alphabet Puzzle Board", "বাংলা বর্ণমালা পাজল বোর্ড", 42_000, Some(55_000), 58, 450, "toys", Some("স্বরবর্ণ ও ব্যঞ্জনবর্ণ knobs for small hands.")),
    p("SH-BABY-001", "Baby Stroller — One-Hand Fold", "শিশু স্ট্রলার — এক হাতে ভাঁজ", 685_000, Some(750_000), 8, 7_400, "baby-kids", Some("Five-point harness, large canopy, up to 22kg.")),
    p("SH-BABY-002", "Baby Carrier — Ergonomic", "শিশু ক্যারিয়ার — আর্গোনমিক", 185_000, None, 22, 620, "baby-kids", Some("Hip-healthy seat, 3 carry positions.")),
    p("SH-BABY-003", "Feeding Bottle Set — 3 pcs Anti-Colic", "ফিডিং বোতল সেট — ৩টি অ্যান্টি-কোলিক", 55_000, Some(68_000), 71, 300, "baby-kids", Some("BPA-free, slow-flow teats included.")),
];

/// The whole demo catalogue: eight stores in display order.
pub const DEMO_CATALOGUE: &[DemoStoreCatalogue] = &[
    DemoStoreCatalogue {
        store: FASHION_STORE,
        products: FASHION_PRODUCTS,
    },
    DemoStoreCatalogue {
        store: GADGET_STORE,
        products: GADGET_PRODUCTS,
    },
    DemoStoreCatalogue {
        store: GROCERY_STORE,
        products: GROCERY_PRODUCTS,
    },
    DemoStoreCatalogue {
        store: HOME_STORE,
        products: HOME_PRODUCTS,
    },
    DemoStoreCatalogue {
        store: BEAUTY_STORE,
        products: BEAUTY_PRODUCTS,
    },
    DemoStoreCatalogue {
        store: AGRO_STORE,
        products: AGRO_PRODUCTS,
    },
    DemoStoreCatalogue {
        store: SPORTS_STORE,
        products: SPORTS_PRODUCTS,
    },
    DemoStoreCatalogue {
        store: BABY_STORE,
        products: BABY_PRODUCTS,
    },
];

/// The catalogue's total product count (the seed report
/// and the integration test both pin against it).
pub fn demo_product_count() -> usize {
    DEMO_CATALOGUE
        .iter()
        .map(|entry| entry.products.len())
        .sum()
}

-- Wave 56: the seed taxonomy.
--
-- Reference data, in a migration on purpose: the taxonomy
-- must exist exactly once on every database (production,
-- demo, every CI test database) and migrations are the one
-- mechanism this repo has that runs exactly once, in order.
-- (No earlier migration carries data — this is the first,
-- and it is data the platform ships WITH, not data a user
-- created.) `ON CONFLICT (slug) DO NOTHING` keeps the file
-- idempotent if it is ever re-applied by hand.
--
-- Shape: 11 top-level categories, each with real children
-- and a few grandchildren (the depth-3 cap exercised, not
-- just allowed). Names are real Bangladesh-marketplace
-- names in English + Bangla; every `name_en` is unique
-- across the whole tree so the Wave 56 text-resolution rule
-- (and the 0062 backfill) can never be ambiguous. `others`
-- is the deliberate catch-all leaf the seller picker points
-- to when nothing fits. Fashion and Electronics start
-- featured (the home strip's default merchandising).
INSERT INTO categories (slug, parent_id, name_en, name_bn, icon_key, sort_order, is_featured) VALUES
    ('fashion', NULL, 'Fashion', 'ফ্যাশন', 'shirt', 10, true),
    ('electronics', NULL, 'Electronics', 'ইলেকট্রনিক্স', 'phone', 20, true),
    ('grocery', NULL, 'Grocery', 'মুদি বাজার', 'cart', 30, false),
    ('home-living', NULL, 'Home & Living', 'ঘর ও সাজসজ্জা', 'home', 40, false),
    ('beauty-health', NULL, 'Beauty & Health', 'সৌন্দর্য ও স্বাস্থ্য', 'sparkles', 50, false),
    ('baby-kids', NULL, 'Baby & Kids', 'শিশু ও বাচ্চা', 'baby', 60, false),
    ('sports-fitness', NULL, 'Sports & Fitness', 'খেলাধুলা ও ফিটনেস', 'trophy', 70, false),
    ('motors', NULL, 'Motors', 'মোটরযান', 'car', 80, false),
    ('agriculture', NULL, 'Agriculture', 'কৃষি', 'leaf', 90, false),
    ('books-stationery', NULL, 'Books & Stationery', 'বই ও স্টেশনারি', 'book', 100, false),
    ('others', NULL, 'Others', 'অন্যান্য', 'grid', 110, false)
ON CONFLICT (slug) DO NOTHING;

INSERT INTO categories (slug, parent_id, name_en, name_bn, icon_key, sort_order) VALUES
    ('mens-fashion', (SELECT id FROM categories WHERE slug = 'fashion'), 'Men''s Fashion', 'পুরুষদের ফ্যাশন', 'shirt', 10),
    ('womens-fashion', (SELECT id FROM categories WHERE slug = 'fashion'), 'Women''s Fashion', 'মহিলাদের ফ্যাশন', 'shirt', 20),
    ('footwear', (SELECT id FROM categories WHERE slug = 'fashion'), 'Footwear', 'জুতা', 'shoe', 30),
    ('watches-accessories', (SELECT id FROM categories WHERE slug = 'fashion'), 'Watches & Accessories', 'ঘড়ি ও আনুষঙ্গিক', 'watch', 40),
    ('mobile-phones', (SELECT id FROM categories WHERE slug = 'electronics'), 'Mobile Phones', 'মোবাইল ফোন', 'phone', 10),
    ('mobile-accessories', (SELECT id FROM categories WHERE slug = 'electronics'), 'Mobile Accessories', 'মোবাইল আনুষঙ্গিক', 'phone', 20),
    ('computers', (SELECT id FROM categories WHERE slug = 'electronics'), 'Computers', 'কম্পিউটার', 'laptop', 30),
    ('home-appliances', (SELECT id FROM categories WHERE slug = 'electronics'), 'Home Appliances', 'গৃহস্থালি যন্ত্রপাতি', 'home', 40),
    ('audio', (SELECT id FROM categories WHERE slug = 'electronics'), 'Audio', 'অডিও', 'phone', 50),
    ('rice-grains', (SELECT id FROM categories WHERE slug = 'grocery'), 'Rice & Grains', 'চাল ও শস্য', 'basket', 10),
    ('spices', (SELECT id FROM categories WHERE slug = 'grocery'), 'Spices', 'মসলা', 'basket', 20),
    ('edible-oil', (SELECT id FROM categories WHERE slug = 'grocery'), 'Edible Oil', 'ভোজ্য তেল', 'basket', 30),
    ('snacks-drinks', (SELECT id FROM categories WHERE slug = 'grocery'), 'Snacks & Drinks', 'নাশতা ও পানীয়', 'cart', 40),
    ('furniture', (SELECT id FROM categories WHERE slug = 'home-living'), 'Furniture', 'আসবাব', 'home', 10),
    ('kitchen', (SELECT id FROM categories WHERE slug = 'home-living'), 'Kitchen', 'রান্নাঘর', 'home', 20),
    ('bedding', (SELECT id FROM categories WHERE slug = 'home-living'), 'Bedding', 'বিছানাপত্র', 'home', 30),
    ('lighting', (SELECT id FROM categories WHERE slug = 'home-living'), 'Lighting', 'আলো', 'sparkles', 40),
    ('skincare', (SELECT id FROM categories WHERE slug = 'beauty-health'), 'Skincare', 'ত্বকের যত্ন', 'sparkles', 10),
    ('makeup', (SELECT id FROM categories WHERE slug = 'beauty-health'), 'Makeup', 'মেকআপ', 'sparkles', 20),
    ('personal-care', (SELECT id FROM categories WHERE slug = 'beauty-health'), 'Personal Care', 'ব্যক্তিগত যত্ন', 'heart', 30),
    ('baby-food', (SELECT id FROM categories WHERE slug = 'baby-kids'), 'Baby Food', 'শিশু খাদ্য', 'baby', 10),
    ('diapers', (SELECT id FROM categories WHERE slug = 'baby-kids'), 'Diapers', 'ডায়াপার', 'baby', 20),
    ('kids-fashion', (SELECT id FROM categories WHERE slug = 'baby-kids'), 'Kids'' Fashion', 'বাচ্চাদের পোশাক', 'shirt', 30),
    ('toys', (SELECT id FROM categories WHERE slug = 'baby-kids'), 'Toys', 'খেলনা', 'gift', 40),
    ('cricket', (SELECT id FROM categories WHERE slug = 'sports-fitness'), 'Cricket', 'ক্রিকেট', 'trophy', 10),
    ('football', (SELECT id FROM categories WHERE slug = 'sports-fitness'), 'Football', 'ফুটবল', 'trophy', 20),
    ('exercise-equipment', (SELECT id FROM categories WHERE slug = 'sports-fitness'), 'Exercise Equipment', 'ব্যায়ামের সরঞ্জাম', 'trophy', 30),
    ('motorcycles', (SELECT id FROM categories WHERE slug = 'motors'), 'Motorcycles', 'মোটরসাইকেল', 'car', 10),
    ('car-accessories', (SELECT id FROM categories WHERE slug = 'motors'), 'Car Accessories', 'গাড়ির আনুষঙ্গিক', 'car', 20),
    ('bicycle', (SELECT id FROM categories WHERE slug = 'motors'), 'Bicycle', 'সাইকেল', 'car', 30),
    ('seeds', (SELECT id FROM categories WHERE slug = 'agriculture'), 'Seeds', 'বীজ', 'leaf', 10),
    ('fertilizer', (SELECT id FROM categories WHERE slug = 'agriculture'), 'Fertilizer', 'সার', 'leaf', 20),
    ('farm-tools', (SELECT id FROM categories WHERE slug = 'agriculture'), 'Farm Tools', 'কৃষি যন্ত্রপাতি', 'leaf', 30),
    ('academic-books', (SELECT id FROM categories WHERE slug = 'books-stationery'), 'Academic Books', 'পাঠ্যবই', 'book', 10),
    ('islamic-books', (SELECT id FROM categories WHERE slug = 'books-stationery'), 'Islamic Books', 'ইসলামিক বই', 'book', 20),
    ('office-supplies', (SELECT id FROM categories WHERE slug = 'books-stationery'), 'Office Supplies', 'অফিস সরঞ্জাম', 'book', 30)
ON CONFLICT (slug) DO NOTHING;

INSERT INTO categories (slug, parent_id, name_en, name_bn, icon_key, sort_order) VALUES
    ('panjabi', (SELECT id FROM categories WHERE slug = 'mens-fashion'), 'Panjabi', 'পাঞ্জাবি', 'shirt', 10),
    ('shirts-t-shirts', (SELECT id FROM categories WHERE slug = 'mens-fashion'), 'Shirts & T-Shirts', 'শার্ট ও টি-শার্ট', 'shirt', 20),
    ('saree', (SELECT id FROM categories WHERE slug = 'womens-fashion'), 'Saree', 'শাড়ি', 'shirt', 10),
    ('salwar-kameez', (SELECT id FROM categories WHERE slug = 'womens-fashion'), 'Salwar Kameez', 'সালোয়ার কামিজ', 'shirt', 20),
    ('laptops', (SELECT id FROM categories WHERE slug = 'computers'), 'Laptops', 'ল্যাপটপ', 'laptop', 10),
    ('computer-accessories', (SELECT id FROM categories WHERE slug = 'computers'), 'Computer Accessories', 'কম্পিউটার আনুষঙ্গিক', 'laptop', 20)
ON CONFLICT (slug) DO NOTHING;

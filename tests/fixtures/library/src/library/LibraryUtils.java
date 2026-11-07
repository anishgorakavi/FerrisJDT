package library;

import java.util.List;

public final class LibraryUtils {
    private LibraryUtils() {
    }

    public static double fineFor(Checkout checkout, int today, double perDayCents) {
        int daysLate = today - checkout.getDueDay();
        if (daysLate <= 0) {
            return 0.0;
        }
        return daysLate * perDayCents / 100.0;
    }

    public static String describe(List<Book> books) {
        StringBuilder out = new StringBuilder();
        for (Book book : books) {
            out.append(book.toString()).append('\n');
        }
        return out.toString();
    }
}
